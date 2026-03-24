use std::collections::HashMap;
use std::path::Path;

use super::permissions;
use crate::component::Component;
use crate::engine::hooks::{self, HookFailureMode};
use crate::engine::shell;
use crate::engine::template::{render_map, TemplateVars};
use crate::error::{Error, Result};
use crate::extension::{
    load_all_extensions, DeployOverride, DeployVerification, ExtensionManifest,
};
use crate::paths as base_path;
use crate::server::SshClient;
use crate::version;

use super::transfer::scp_file;
use super::types::DeployResult;

/// Detect if a component's artifact is a CLI binary matching the currently
/// running process name. Used to print a post-deploy hint for self-deploy.
pub(super) fn is_self_deploy(component: &Component) -> bool {
    let artifact_pattern = match component.build_artifact.as_ref() {
        Some(p) => p,
        None => return false,
    };

    let artifact_name = Path::new(artifact_pattern)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    let exe_name = std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));

    match exe_name {
        Some(name) => name == artifact_name,
        None => false,
    }
}

/// For self-deploy components, check if the currently installed binary is newer
/// than the build artifact. Returns the installed binary path if it should be
/// preferred, or None to keep using the build artifact.
///
/// This handles the upgrade-then-deploy scenario: `homeboy upgrade` installs a
/// new binary to e.g. /usr/local/bin/homeboy, but the build artifact at
/// target/release/homeboy is still the old version. Without this check,
/// `deploy --shared` would push the stale build artifact to the fleet.
pub(super) fn prefer_installed_binary(build_artifact: &Path) -> Option<std::path::PathBuf> {
    let exe_path = std::env::current_exe().ok()?;

    // Don't redirect if they're the same file
    if exe_path == build_artifact {
        return None;
    }

    let exe_mtime = exe_path.metadata().ok()?.modified().ok()?;
    let art_mtime = build_artifact.metadata().ok()?.modified().ok()?;

    if exe_mtime > art_mtime {
        log_status!(
            "deploy",
            "Installed binary ({}) is newer than build artifact ({}) — deploying installed binary",
            exe_path.display(),
            build_artifact.display()
        );
        Some(exe_path)
    } else {
        None
    }
}

/// Fetch versions from remote server for components.
pub fn fetch_remote_versions(
    components: &[Component],
    base_path: &str,
    client: &SshClient,
) -> HashMap<String, String> {
    let mut versions = HashMap::new();

    for component in components {
        // Try standard version-file approach first
        if let Some(ver) = fetch_version_from_file(component, base_path, client) {
            versions.insert(component.id.clone(), ver);
            continue;
        }

        // Fallback: for CLI binaries (has build_artifact, no remote_path),
        // try running the binary with --version on the remote server.
        if let Some(ver) = fetch_version_from_binary(component, client) {
            versions.insert(component.id.clone(), ver);
        }
    }

    versions
}

/// Try to fetch version by reading a version file on the remote server.
fn fetch_version_from_file(
    component: &Component,
    base_path: &str,
    client: &SshClient,
) -> Option<String> {
    let version_file = component
        .version_targets
        .as_ref()
        .and_then(|targets| targets.first())
        .map(|t| t.file.as_str())?;

    let remote_path =
        base_path::join_remote_child(Some(base_path), &component.remote_path, version_file).ok()?;

    let output = client.execute(&format!("cat '{}' 2>/dev/null", remote_path));

    if output.success {
        let pattern = component
            .version_targets
            .as_ref()
            .and_then(|targets| targets.first())
            .and_then(|t| t.pattern.as_deref());

        parse_component_version(&output.stdout, pattern, version_file)
    } else {
        None
    }
}

/// Try to fetch version by running `<binary> --version` on the remote server.
///
/// This handles CLI binary components (like homeboy itself) that are installed
/// as executables without a parseable version file on the remote server.
fn fetch_version_from_binary(component: &Component, client: &SshClient) -> Option<String> {
    let artifact = component.build_artifact.as_ref()?;

    // Extract binary name from build_artifact path (e.g., "target/release/homeboy" → "homeboy")
    let binary_name = Path::new(artifact).file_name()?.to_str()?;

    // Try common install locations
    let candidates = [
        format!("/usr/local/bin/{}", binary_name),
        format!("/usr/bin/{}", binary_name),
        binary_name.to_string(), // Relies on PATH
    ];

    for candidate in &candidates {
        let output = client.execute(&format!(
            "{} --version 2>/dev/null",
            shell::quote_path(candidate)
        ));
        if output.success {
            let stdout = output.stdout.trim();
            // Parse "binary_name X.Y.Z" or just "X.Y.Z"
            if let Some(ver) = parse_cli_version_output(stdout) {
                return Some(ver);
            }
        }
    }

    None
}

/// Parse version from CLI `--version` output.
///
/// Handles common formats:
/// - "homeboy 0.71.1"
/// - "v0.71.1"
/// - "0.71.1"
fn parse_cli_version_output(output: &str) -> Option<String> {
    // Try "name X.Y.Z" pattern (e.g., "homeboy 0.71.1")
    let re = regex::Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    re.find(output).map(|m| m.as_str().to_string())
}

/// Parse version from content using pattern or extension defaults.
fn parse_component_version(content: &str, pattern: Option<&str>, filename: &str) -> Option<String> {
    let pattern_str = match pattern {
        Some(p) => p.replace("\\\\", "\\"),
        None => version::default_pattern_for_file(filename)?,
    };

    version::parse_version(content, &pattern_str)
}

/// Find deploy verification config from extensions.
pub(super) fn find_deploy_verification(target_path: &str) -> Option<DeployVerification> {
    for extension in load_all_extensions().unwrap_or_default() {
        for verification in extension.deploy_verifications() {
            if target_path.contains(&verification.path_pattern) {
                return Some(verification.clone());
            }
        }
    }
    None
}

/// Find deploy override config from extensions.
pub(super) fn find_deploy_override(
    target_path: &str,
) -> Option<(DeployOverride, ExtensionManifest)> {
    for extension in load_all_extensions().unwrap_or_default() {
        for override_config in extension.deploy_overrides() {
            if target_path.contains(&override_config.path_pattern) {
                return Some((override_config.clone(), extension));
            }
        }
    }
    None
}

/// Deploy using extension-defined override strategy.
#[allow(clippy::too_many_arguments)]
pub(super) fn deploy_with_override(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
    override_config: &DeployOverride,
    extension: &ExtensionManifest,
    verification: Option<&DeployVerification>,
    site_root: Option<&str>,
    domain: Option<&str>,
    remote_owner: Option<&str>,
) -> Result<DeployResult> {
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
        })?;

    let staging_artifact = format!("{}/{}", override_config.staging_path, artifact_filename);

    // Step 1: Create staging directory
    let mkdir_cmd = format!(
        "mkdir -p {}",
        shell::quote_path(&override_config.staging_path)
    );
    log_status!(
        "deploy",
        "Using extension deploy override: {}",
        extension.id
    );
    log_status!(
        "deploy",
        "Creating staging directory: {}",
        override_config.staging_path
    );
    let mkdir_output = ssh_client.execute(&mkdir_cmd);
    if !mkdir_output.success {
        return Ok(DeployResult::failure(
            mkdir_output.exit_code,
            format!(
                "Failed to create staging directory: {}",
                mkdir_output.stderr
            ),
        ));
    }

    // Step 2: Upload artifact to staging
    let upload_result = scp_file(ssh_client, local_path, &staging_artifact)?;
    if !upload_result.success {
        return Ok(upload_result);
    }

    // Step 3: Render and execute install command
    let cli_path = extension
        .cli
        .as_ref()
        .and_then(|c| c.default_cli_path.as_deref())
        .unwrap_or("wp");

    let mut vars = HashMap::new();
    vars.insert("artifact".to_string(), artifact_filename.to_string());
    vars.insert("stagingArtifact".to_string(), staging_artifact.clone());
    vars.insert("targetDir".to_string(), remote_path.to_string());
    vars.insert("siteRoot".to_string(), site_root.unwrap_or("").to_string());
    vars.insert("cliPath".to_string(), cli_path.to_string());
    vars.insert("domain".to_string(), domain.unwrap_or("").to_string());
    vars.insert(
        "allowRootFlag".to_string(),
        if ssh_client.user == "root" {
            "--allow-root"
        } else {
            ""
        }
        .to_string(),
    );

    let install_cmd = render_map(&override_config.install_command, &vars);
    log_status!("deploy", "Running install command: {}", install_cmd);

    let install_output = ssh_client.execute(&install_cmd);
    if !install_output.success {
        let error_detail = if install_output.stderr.is_empty() {
            install_output.stdout.clone()
        } else {
            install_output.stderr.clone()
        };
        return Ok(DeployResult::failure(
            install_output.exit_code,
            format!(
                "Install command failed (exit {}): {}",
                install_output.exit_code, error_detail
            ),
        ));
    }

    // Step 4: Run cleanup command if configured
    if let Some(cleanup_cmd_template) = &override_config.cleanup_command {
        let cleanup_cmd = render_map(cleanup_cmd_template, &vars);
        log_status!("deploy", "Running cleanup: {}", cleanup_cmd);
        let _ = ssh_client.execute(&cleanup_cmd); // Best effort cleanup
    }

    // Step 5: Fix permissions unless skipped
    if !override_config.skip_permissions_fix {
        log_status!("deploy", "Fixing file permissions");
        permissions::fix_deployed_permissions(ssh_client, remote_path, remote_owner)?;
    }

    // Step 6: Run verification if configured
    if let Some(v) = verification {
        if let Some(ref verify_cmd_template) = v.verify_command {
            let mut verify_vars = HashMap::new();
            verify_vars.insert(
                TemplateVars::TARGET_DIR.to_string(),
                remote_path.to_string(),
            );
            let verify_cmd = render_map(verify_cmd_template, &verify_vars);

            let verify_output = ssh_client.execute(&verify_cmd);
            if !verify_output.success || verify_output.stdout.trim().is_empty() {
                let error_msg = v
                    .verify_error_message
                    .as_ref()
                    .map(|msg| render_map(msg, &verify_vars))
                    .unwrap_or_else(|| format!("Deploy verification failed for {}", remote_path));
                return Ok(DeployResult::failure(1, error_msg));
            }
        }
    }

    Ok(DeployResult::success(0))
}

/// Build template variables and run `post:deploy` hooks remotely via SSH.
///
/// This is a convenience wrapper around `hooks::run_hooks_remote` that builds
/// the standard deploy template variables and runs hooks non-fatally (failures
/// are logged but do not abort the deploy).
pub(super) fn run_post_deploy_hooks(
    ssh_client: &SshClient,
    component: &Component,
    install_dir: &str,
    base_path: &str,
) {
    let mut vars = HashMap::new();
    vars.insert(TemplateVars::COMPONENT_ID.to_string(), component.id.clone());
    vars.insert(
        TemplateVars::INSTALL_DIR.to_string(),
        install_dir.to_string(),
    );
    vars.insert(TemplateVars::BASE_PATH.to_string(), base_path.to_string());

    match hooks::run_hooks_remote(
        ssh_client,
        component,
        hooks::events::POST_DEPLOY,
        HookFailureMode::NonFatal,
        &vars,
    ) {
        Ok(result) => {
            for cmd_result in &result.commands {
                if cmd_result.success {
                    log_status!("deploy", "post:deploy> {}", cmd_result.command);
                } else {
                    log_status!(
                        "deploy",
                        "post:deploy failed (exit {})> {}",
                        cmd_result.exit_code,
                        cmd_result.command
                    );
                }
            }
        }
        Err(e) => {
            log_status!("deploy", "post:deploy hook error: {}", e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_self_deploy_some_p_p() {
        let _result = is_self_deploy();
    }

    #[test]
    fn test_is_self_deploy_none_return_false() {
        let result = is_self_deploy();
        assert!(!result, "expected false when: None => return false,");
    }

    #[test]
    fn test_is_self_deploy_match_exe_name() {
        let _result = is_self_deploy();
    }

    #[test]
    fn test_prefer_installed_binary_default_path() {
        let _result = prefer_installed_binary();
    }

    #[test]
    fn test_prefer_installed_binary_exe_path_build_artifact() {
        let result = prefer_installed_binary();
        assert!(
            result.is_none(),
            "expected None for: exe_path == build_artifact"
        );
    }

    #[test]
    fn test_prefer_installed_binary_exe_path_build_artifact_2() {
        let _result = prefer_installed_binary();
    }

    #[test]
    fn test_prefer_installed_binary_exe_path_build_artifact_3() {
        let _result = prefer_installed_binary();
    }

    #[test]
    fn test_prefer_installed_binary_some_exe_path() {
        let result = prefer_installed_binary();
        assert!(result.is_some(), "expected Some for: Some(exe_path)");
    }

    #[test]
    fn test_prefer_installed_binary_else() {
        let result = prefer_installed_binary();
        assert!(result.is_none(), "expected None for: else");
    }

    #[test]
    fn test_prefer_installed_binary_has_expected_effects() {
        // Expected effects: logging

        let _ = prefer_installed_binary();
    }

    #[test]
    fn test_fetch_remote_versions_if_let_some_ver_fetch_version_from_file_component_base_path_() {
        let components = Vec::new();
        let base_path = "";
        let client = Default::default();
        let result = fetch_remote_versions(&components, &base_path, &client);
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(ver) = fetch_version_from_file(component, base_path, client) {{");
    }

    #[test]
    fn test_fetch_remote_versions_if_let_some_ver_fetch_version_from_binary_component_client() {
        let components = Vec::new();
        let base_path = "";
        let client = Default::default();
        let result = fetch_remote_versions(&components, &base_path, &client);
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(ver) = fetch_version_from_binary(component, client) {{");
    }

    #[test]
    fn test_fetch_remote_versions_has_expected_effects() {
        // Expected effects: mutation
        let components = Vec::new();
        let base_path = "";
        let client = Default::default();
        let _ = fetch_remote_versions(&components, &base_path, &client);
    }

    #[test]
    fn test_find_deploy_verification_target_path_contains_verification_path_pattern() {
        let result = find_deploy_verification();
        let inner =
            result.expect("expected Some for: target_path.contains(&verification.path_pattern)");
        // Branch returns Some(verification.clone()
        assert_eq!(inner.path_pattern, String::new());
        assert_eq!(inner.verify_command, None);
        assert_eq!(inner.verify_error_message, None);
    }

    #[test]
    fn test_find_deploy_verification_target_path_contains_verification_path_pattern_2() {
        let result = find_deploy_verification();
        assert!(
            result.is_none(),
            "expected None for: target_path.contains(&verification.path_pattern)"
        );
    }

    #[test]
    fn test_find_deploy_override_target_path_contains_override_config_path_pattern() {
        let result = find_deploy_override();
        assert!(
            result.is_some(),
            "expected Some for: target_path.contains(&override_config.path_pattern)"
        );
    }

    #[test]
    fn test_find_deploy_override_target_path_contains_override_config_path_pattern_2() {
        let result = find_deploy_override();
        assert!(
            result.is_none(),
            "expected None for: target_path.contains(&override_config.path_pattern)"
        );
    }

    #[test]
    fn test_deploy_with_override_some_local_path_display_to_string() {
        let _result = deploy_with_override();
    }

    #[test]
    fn test_deploy_with_override_default_path() {
        let _result = deploy_with_override();
    }

    #[test]
    fn test_deploy_with_override_default_path_2() {
        let _result = deploy_with_override();
    }

    #[test]
    fn test_deploy_with_override_upload_result_success() {
        let result = deploy_with_override();
        let inner = result.unwrap();
        // Branch returns Ok(upload_result) when: !upload_result.success
        assert_eq!(inner.success, false);
        assert_eq!(inner.exit_code, 0);
        assert_eq!(inner.error, None);
    }

    #[test]
    fn test_deploy_with_override_if_let_some_cleanup_cmd_template_override_config_cleanup_com() {
        let _result = deploy_with_override();
    }

    #[test]
    fn test_deploy_with_override_override_config_skip_permissions_fix() {
        let _result = deploy_with_override();
    }

    #[test]
    fn test_deploy_with_override_if_let_some_v_verification() {
        let _result = deploy_with_override();
    }

    #[test]
    fn test_deploy_with_override_let_some_v_verification() {
        let _result = deploy_with_override();
    }

    #[test]
    fn test_deploy_with_override_default_path_3() {
        let result = deploy_with_override();
        let inner = result.unwrap();
        // Branch returns Ok(DeployResult::failure(1, error_msg) when: default path
        assert_eq!(inner.success, false);
        assert_eq!(inner.exit_code, 0);
        assert_eq!(inner.error, None);
    }

    #[test]
    fn test_deploy_with_override_ok_deployresult_success_0() {
        let result = deploy_with_override();
        let inner = result.unwrap();
        // Branch returns Ok(DeployResult::success(0) when: Ok(DeployResult::success(0))
        assert_eq!(inner.success, false);
        assert_eq!(inner.exit_code, 0);
        assert_eq!(inner.error, None);
    }

    #[test]
    fn test_deploy_with_override_has_expected_effects() {
        // Expected effects: logging, mutation

        let _ = deploy_with_override();
    }

    #[test]
    fn test_run_post_deploy_hooks_ok_result() {
        run_post_deploy_hooks();
    }

    #[test]
    fn test_run_post_deploy_hooks_err_e() {
        run_post_deploy_hooks();
    }

    #[test]
    fn test_run_post_deploy_hooks_has_expected_effects() {
        // Expected effects: logging, mutation

        let _ = run_post_deploy_hooks();
    }
}
