use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::permissions;
use crate::core::component::{Component, VersionTarget};
use crate::core::engine::hooks::{self, HookFailureMode};
use crate::core::engine::shell;
use crate::core::engine::template::{render_map, TemplateVars};
use crate::core::error::{Error, Result};
use crate::core::extension::{
    load_all_extensions, DeployArchiveInstallPolicy, DeployOverride, DeployVerification,
    ExtensionManifest,
};
use crate::core::paths as base_path;
use crate::core::project::Project;
use crate::core::release::version;
use crate::core::server::SshClient;

use super::path_roots::resolve_effective_remote_path;
use super::transfer::scp_file;
use super::types::{DeployEffect, DeployResult};

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
    fetch_remote_versions_for_project(components, None, base_path, client)
}

pub(super) fn fetch_remote_versions_for_project(
    components: &[Component],
    project: Option<&Project>,
    base_path: &str,
    client: &SshClient,
) -> HashMap<String, String> {
    let mut versions = HashMap::new();

    for component in components {
        // Try standard version-file approach first
        if let Some(ver) = fetch_version_from_file(component, project, base_path, client) {
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
    project: Option<&Project>,
    base_path: &str,
    client: &SshClient,
) -> Option<String> {
    let version_targets = component.version_targets.as_ref()?;

    let remote_dir = match project {
        Some(project) => resolve_effective_remote_path(project, component, base_path).ok()?,
        None => base_path::join_remote_path(Some(base_path), &component.remote_path).ok()?,
    };

    for target in version_targets {
        for remote_file in remote_version_file_candidates(target) {
            let remote_path = base_path::join_remote_child(None, &remote_dir, &remote_file).ok()?;
            let pattern = target.pattern.as_deref();

            if client.is_local {
                if let Ok(content) = fs::read_to_string(&remote_path) {
                    if let Some(version) = parse_component_version(&content, pattern, &remote_file)
                    {
                        return Some(version);
                    }
                }

                continue;
            }

            let output = client.execute(&format!("cat '{}' 2>/dev/null", remote_path));
            if output.success {
                if let Some(version) =
                    parse_component_version(&output.stdout, pattern, &remote_file)
                {
                    return Some(version);
                }
            }
        }
    }

    None
}

fn remote_version_file_candidates(target: &VersionTarget) -> Vec<String> {
    let mut candidates = vec![target.file.clone()];
    if let Some(file_name) = Path::new(&target.file)
        .file_name()
        .and_then(|name| name.to_str())
    {
        if file_name != target.file {
            candidates.push(file_name.to_string());
        }
    }

    candidates
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
        for policy in extension.deploy_archive_installs() {
            if target_path.contains(&policy.path_pattern) {
                return archive_install_verification(policy);
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
        for policy in extension.deploy_archive_installs() {
            if target_path.contains(&policy.path_pattern) {
                return Some((archive_install_override(policy), extension));
            }
        }
    }
    None
}

fn archive_install_override(policy: &DeployArchiveInstallPolicy) -> DeployOverride {
    let root_check = if policy.root_must_match_target_basename {
        " && target_slug=$(basename \"$target\") && if [ \"$zip_root\" != \"$target_slug\" ]; then echo \"ERROR: archive root $zip_root does not match target basename $target_slug\" && exit 1; fi"
    } else {
        ""
    };

    // Robust, filesystem-portable install/replace flow.
    //
    // Why not `mv` from the staging dir (e.g. /tmp) into the target? On some
    // sandboxed/managed filesystems (notably WP Cloud) `/tmp` lives on a
    // different filesystem than the deploy target. A cross-device `mv` falls
    // back to a recursive copy that tries to preserve permissions/metadata for
    // every file. For plugins containing deep nested Node package trees
    // (node_modules, vendor sub-packages with thousands of small files and
    // symlinks) that copy fails with "Operation not permitted" or even a
    // seccomp "Bad system call" abort. See issue #3027.
    //
    // Instead we stage extraction into a temp dir that sits *adjacent* to the
    // target (same parent directory => same filesystem). Every subsequent
    // `mv` (extracted tree -> target, existing target -> backup, restore on
    // failure) is then a single intra-filesystem rename(2): O(1), no recursion,
    // no per-file metadata copies, regardless of how deep the directory tree is.
    //
    // Flow:
    //   1. Resolve + validate the archive root directory name.
    //   2. Create an adjacent temp dir and extract the artifact into it.
    //   3. Locate the extracted root inside the temp dir.
    //   4. Back up the existing target via same-filesystem rename.
    //   5. Atomically rename the extracted tree into place.
    //   6. On any failure, restore the backup; always clean up the temp dir.
    //   7. On success, remove the backup.
    DeployOverride {
        path_pattern: policy.path_pattern.clone(),
        staging_path: policy.staging_path.clone(),
        install_command: format!(
            "target=\"{{{{targetDir}}}}\" \
&& parent=\"{{{{targetParentDir}}}}\" \
&& backup=\"$target.homeboy-bak.$$\" \
&& zip_root=$(unzip -Z1 \"{{{{stagingArtifact}}}}\" | awk -F/ 'NF && $1 != \"\" {{ print $1; exit }}') \
&& if [ -z \"$zip_root\" ]; then echo 'ERROR: Could not determine archive root directory' && exit 1; fi{root_check} \
&& mkdir -p \"$parent\" \
&& staged=$(mktemp -d \"{{{{targetAdjacentTempPattern}}}}\") || {{ echo 'ERROR: could not create adjacent staging dir' && exit 1; }} \
&& trap 'rm -rf \"$staged\"' EXIT \
&& if ! unzip -oq \"{{{{stagingArtifact}}}}\" -d \"$staged\"; then echo 'ERROR: archive extraction failed' && exit 1; fi \
&& extracted=\"$staged/$zip_root\" \
&& if [ ! -d \"$extracted\" ]; then echo \"ERROR: expected directory $zip_root not found in archive\" && exit 1; fi \
&& rm -rf \"$backup\" \
&& if [ -e \"$target\" ]; then echo \"Backing up existing target to $backup\" && mv \"$target\" \"$backup\" || {{ echo 'ERROR: could not back up existing target' && exit 1; }}; fi \
&& if ! mv \"$extracted\" \"$target\"; then \
echo 'ERROR: archive install failed — restoring backup' \
&& rm -rf \"$target\" \
&& if [ -e \"$backup\" ]; then mv \"$backup\" \"$target\" || true; fi \
&& exit 1; \
fi \
&& rm -rf \"$backup\" \
&& test -d \"$target\" || {{ echo 'ERROR: target missing after install' && exit 1; }}"
        ),
        cleanup_command: Some(
            "rm -f \"{{stagingArtifact}}\" && rm -rf \"{{targetDir}}.homeboy-bak.\"*".to_string(),
        ),
        skip_permissions_fix: policy.skip_permissions_fix,
    }
}

fn archive_install_verification(policy: &DeployArchiveInstallPolicy) -> Option<DeployVerification> {
    let header = policy.required_header.as_ref()?;
    let selector = if let Some(file) = header.file.as_deref() {
        format!(
            "candidate=$(printf '%s\\n' \"$entries\" | awk -v required={} '{{ slash = index($0, \"/\"); rel = slash ? substr($0, slash + 1) : $0; if (rel == required) {{ print $0; exit }} }}')",
            shell::quote_arg(file)
        )
    } else if let Some(file_glob) = header.file_glob.as_deref() {
        let file_regex = glob_to_awk_regex(file_glob);
        format!(
            "candidate=$(printf '%s\\n' \"$entries\" | awk -v pattern={} '{{ slash = index($0, \"/\"); rel = slash ? substr($0, slash + 1) : $0; count = split(rel, parts, \"/\"); base = parts[count]; if (base ~ pattern) {{ print $0; exit }} }}')",
            shell::quote_arg(&file_regex)
        )
    } else {
        return None;
    };

    let root_check = if policy.root_must_match_target_basename {
        " && target_slug=$(basename \"{{targetDir}}\") && [ \"$zip_root\" = \"$target_slug\" ]"
    } else {
        ""
    };
    let contains = shell::quote_arg(&header.contains);

    Some(DeployVerification {
        path_pattern: policy.path_pattern.clone(),
        verify_command: Some(format!(
            "test -f \"{{{{stagingArtifact}}}}\" && entries=$(unzip -Z1 \"{{{{stagingArtifact}}}}\") && zip_root=$(printf '%s\\n' \"$entries\" | awk -F/ 'NF && $1 != \"\" {{ print $1; exit }}'){root_check} && {selector} && test -n \"$candidate\" && rel=\"$candidate\" && case \"$rel\" in */*) rel=\"${{rel#*/}}\" ;; esac && test -f \"{{{{targetDir}}}}/$rel\" && unzip -p \"{{{{stagingArtifact}}}}\" \"$candidate\" | cmp -s - \"{{{{targetDir}}}}/$rel\" && grep -F -l {contains} \"{{{{targetDir}}}}/$rel\" 2>/dev/null"
        )),
        verify_error_message: Some(
            "Archive install verification failed for {{targetDir}}: installed header file does not match {{stagingArtifact}} or required header was not found"
                .to_string(),
        ),
    })
}

fn glob_to_awk_regex(glob: &str) -> String {
    let mut regex = String::from("^");
    for ch in glob.chars() {
        match ch {
            '*' => regex.push_str(".*"),
            '?' => regex.push('.'),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '{' | '}' | '[' | ']' | '\\' => {
                regex.push('\\');
                regex.push(ch);
            }
            _ => regex.push(ch),
        }
    }
    regex.push('$');
    regex
}

/// Detect whether the current deploy target exists as a symlink.
///
/// A dev/rig workflow may symlink an external checkout into the deploy location.
/// When an override install replaces that symlink with a real directory, the
/// resolved (real) path of the target changes. Surfacing this lets callers and
/// environments that cache a path mapping for the old destination know they may
/// need to refresh/recycle. This stays generic: it makes no assumptions about
/// the runtime that owns the target or how (or whether) it caches paths.
fn target_is_symlink(ssh_client: &SshClient, target_path: &str) -> bool {
    if target_path.is_empty() {
        return false;
    }
    ssh_client
        .execute(&format!("test -L {}", shell::quote_path(target_path)))
        .success
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
    cli_path_override: Option<&str>,
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
    let mut verified = false;

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
    // Resolution order: component/project cli_path override → extension default → "wp"
    let cli_path = cli_path_override
        .or_else(|| {
            extension
                .cli
                .as_ref()
                .and_then(|c| c.default_cli_path.as_deref())
        })
        .unwrap_or("wp");

    let vars = deploy_override_template_vars(
        artifact_filename,
        &staging_artifact,
        remote_path,
        site_root,
        cli_path,
        domain,
    );

    // Surface a symlink -> real-directory transition before the swap happens. A
    // dev/rig workflow may symlink an external checkout into the deploy location;
    // the install command atomically replaces that symlink with a real directory,
    // which changes the resolved (real) path of the target. Environments caching
    // a path mapping for the old symlink destination may need to be recycled or
    // refreshed to pick up the new location. Generic: no assumptions about the
    // target runtime or its caching behavior.
    if target_is_symlink(ssh_client, remote_path) {
        log_status!(
            "deploy",
            "⚠ Deploy target {} is currently a symlink; the install will replace it with a real directory. Any environment that caches the symlink's resolved path may need to be refreshed or recycled to pick up the new location.",
            remote_path
        );
    }

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

    // Step 4: Fix permissions unless skipped
    if !override_config.skip_permissions_fix {
        log_status!("deploy", "Fixing file permissions");
        permissions::fix_deployed_permissions(ssh_client, remote_path, remote_owner)?;
    }

    // Step 5: Run verification if configured. Keep the staged artifact around
    // until after this step so extension verifiers can compare installed files
    // against the exact uploaded payload.
    if let Some(v) = verification {
        if let Some(ref verify_cmd_template) = v.verify_command {
            let mut verify_vars = vars.clone();
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
            verified = true;
        }
    }

    // Step 6: Run cleanup command if configured
    if let Some(cleanup_cmd_template) = &override_config.cleanup_command {
        let cleanup_cmd = render_map(cleanup_cmd_template, &vars);
        log_status!("deploy", "Running cleanup: {}", cleanup_cmd);
        let _ = ssh_client.execute(&cleanup_cmd); // Best effort cleanup
    }

    Ok(DeployResult::success(0).with_effect(DeployEffect {
        remote_path: remote_path.to_string(),
        artifact_path: Some(staging_artifact),
        verified,
    }))
}

fn deploy_override_template_vars(
    artifact_filename: &str,
    staging_artifact: &str,
    target_dir: &str,
    site_root: Option<&str>,
    cli_path: &str,
    domain: Option<&str>,
) -> HashMap<String, String> {
    let target_parent_dir = target_dir
        .trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let target_basename = target_dir
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    let target_adjacent_temp_pattern = if target_parent_dir.is_empty() {
        String::new()
    } else {
        format!("{}/.homeboy-install.XXXXXX", target_parent_dir)
    };

    HashMap::from([
        ("artifact".to_string(), artifact_filename.to_string()),
        ("stagingArtifact".to_string(), staging_artifact.to_string()),
        (TemplateVars::TARGET_DIR.to_string(), target_dir.to_string()),
        (
            TemplateVars::TARGET_BASENAME.to_string(),
            target_basename.to_string(),
        ),
        (
            TemplateVars::TARGET_PARENT_DIR.to_string(),
            target_parent_dir.to_string(),
        ),
        (
            TemplateVars::TARGET_ADJACENT_TEMP_PATTERN.to_string(),
            target_adjacent_temp_pattern,
        ),
        ("siteRoot".to_string(), site_root.unwrap_or("").to_string()),
        ("cliPath".to_string(), cli_path.to_string()),
        ("domain".to_string(), domain.unwrap_or("").to_string()),
        ("allowRootFlag".to_string(), "--allow-root".to_string()),
    ])
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
    use crate::core::component::VersionTarget;
    use crate::core::extension::{
        DeployArchiveInstallPolicy, DeployOverride, DeployRequiredHeader, DeployVerification,
        ExtensionManifest,
    };
    use crate::core::server::SshClient;
    use std::collections::HashMap;
    use std::fs;
    use std::io::Write;

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

    fn extension() -> ExtensionManifest {
        serde_json::from_value(serde_json::json!({
            "id": "fixture",
            "name": "Fixture",
            "version": "1.0.0"
        }))
        .expect("extension manifest")
    }

    fn versioned_component(remote_path: &str) -> Component {
        Component {
            id: "fixture".to_string(),
            remote_path: remote_path.to_string(),
            version_targets: Some(vec![VersionTarget {
                file: "fixture.php".to_string(),
                pattern: Some(r"Version:\s*(\d+\.\d+\.\d+)".to_string()),
                artifact_path: None,
            }]),
            ..Default::default()
        }
    }

    fn write_zip(path: &Path, files: &[(&str, &str)]) {
        let file = fs::File::create(path).expect("zip file");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();

        for (name, contents) in files {
            zip.start_file(*name, options).expect("zip entry");
            zip.write_all(contents.as_bytes()).expect("zip contents");
        }

        zip.finish().expect("finish zip");
    }

    fn package_archive_policy(staging_path: String) -> DeployArchiveInstallPolicy {
        DeployArchiveInstallPolicy {
            path_pattern: "/packages/".to_string(),
            staging_path,
            root_must_match_target_basename: true,
            required_header: Some(DeployRequiredHeader {
                file: Some("{{targetBasename}}.manifest".to_string()),
                contains: "Package Name:".to_string(),
                file_glob: None,
            }),
            skip_permissions_fix: true,
        }
    }

    fn bundle_archive_policy(staging_path: String) -> DeployArchiveInstallPolicy {
        DeployArchiveInstallPolicy {
            path_pattern: "/bundles/".to_string(),
            staging_path,
            root_must_match_target_basename: true,
            required_header: Some(DeployRequiredHeader {
                file: Some("bundle.manifest".to_string()),
                file_glob: None,
                contains: "Bundle Name:".to_string(),
            }),
            skip_permissions_fix: true,
        }
    }

    #[test]
    fn test_fetch_remote_versions() {
        let temp = tempfile::tempdir().expect("temp dir");
        fs::write(temp.path().join("fixture.php"), "Version: 1.2.3").expect("version file");

        let versions = fetch_remote_versions(
            &[versioned_component(".")],
            temp.path().to_str().expect("base path"),
            &local_client(),
        );

        assert_eq!(versions.get("fixture").map(String::as_str), Some("1.2.3"));
    }

    #[test]
    fn test_fetch_remote_versions_for_project() {
        let temp = tempfile::tempdir().expect("temp dir");
        let remote_dir = temp.path().join("plugin");
        fs::create_dir_all(&remote_dir).expect("remote dir");
        fs::write(remote_dir.join("fixture.php"), "Version: 2.3.4").expect("version file");

        let versions = fetch_remote_versions_for_project(
            &[versioned_component("plugin")],
            None,
            temp.path().to_str().expect("base path"),
            &local_client(),
        );

        assert_eq!(versions.get("fixture").map(String::as_str), Some("2.3.4"));
    }

    #[test]
    fn test_fetch_remote_versions_tries_later_target_basename() {
        let temp = tempfile::tempdir().expect("temp dir");
        let remote_dir = temp.path().join("plugin");
        fs::create_dir_all(&remote_dir).expect("remote dir");
        fs::write(remote_dir.join("fixture.php"), "Version: 3.4.5").expect("version file");

        let component = Component {
            id: "fixture".to_string(),
            remote_path: "plugin".to_string(),
            version_targets: Some(vec![
                VersionTarget {
                    file: "package.json".to_string(),
                    pattern: Some(r#""version":\s*"([0-9.]+)""#.to_string()),
                    artifact_path: None,
                },
                VersionTarget {
                    file: "packages/component/fixture.php".to_string(),
                    pattern: Some(r"Version:\s*([0-9.]+)".to_string()),
                    artifact_path: None,
                },
            ]),
            ..Default::default()
        };

        let versions = fetch_remote_versions_for_project(
            &[component],
            None,
            temp.path().to_str().expect("base path"),
            &local_client(),
        );

        assert_eq!(versions.get("fixture").map(String::as_str), Some("3.4.5"));
    }

    #[test]
    fn test_find_deploy_override() {
        assert!(find_deploy_override("/not-a-real-homeboy-extension-target/").is_none());
    }

    #[test]
    fn test_find_deploy_verification() {
        assert!(find_deploy_verification("/not-a-real-homeboy-extension-target/").is_none());
    }

    #[test]
    fn test_prefer_installed_binary() {
        let current_exe = std::env::current_exe().expect("current exe");

        assert!(prefer_installed_binary(&current_exe).is_none());
    }

    #[test]
    fn test_run_post_deploy_hooks() {
        let component = Component {
            id: "fixture".to_string(),
            ..Default::default()
        };

        run_post_deploy_hooks(&local_client(), &component, "/tmp/fixture", "/tmp");
    }

    #[test]
    fn test_deploy_with_override_keeps_staging_artifact_until_verification() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("artifact.zip");
        let staging = temp.path().join("staging");
        let target = temp.path().join("target");
        fs::write(&artifact, "artifact bytes").expect("artifact");

        let override_config = DeployOverride {
            path_pattern: "/target/".to_string(),
            staging_path: staging.to_string_lossy().to_string(),
            install_command:
                "mkdir -p {{targetDir}} && cp {{stagingArtifact}} {{targetDir}}/installed.zip"
                    .to_string(),
            cleanup_command: Some("rm -f {{stagingArtifact}}".to_string()),
            skip_permissions_fix: true,
        };
        let verification = DeployVerification {
            path_pattern: "/target/".to_string(),
            verify_command: Some(
                "test -f {{stagingArtifact}} && cmp -s {{stagingArtifact}} {{targetDir}}/installed.zip && echo verified"
                    .to_string(),
            ),
            verify_error_message: Some("artifact mismatch".to_string()),
        };

        let result = deploy_with_override(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            &override_config,
            &extension(),
            Some(&verification),
            Some(temp.path().to_str().expect("site root")),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(result.success, "deploy failed: {:?}", result.error);
        assert!(!staging.join("artifact.zip").exists());
        assert_eq!(
            fs::read_to_string(target.join("installed.zip")).expect("installed artifact"),
            "artifact bytes"
        );
    }

    #[test]
    fn test_archive_install_policy_replaces_target_and_verifies_header() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("fixture.zip");
        let staging = temp.path().join("staging");
        let target = temp.path().join("packages/fixture");

        fs::create_dir_all(&target).expect("target dir");
        fs::write(target.join("stale.php"), "stale").expect("stale file");
        write_zip(
            &artifact,
            &[("fixture/fixture.manifest", "Package Name: Fixture\n")],
        );

        let policy = package_archive_policy(staging.to_string_lossy().to_string());
        let override_config = archive_install_override(&policy);
        let verification = archive_install_verification(&policy).expect("verification");

        let result = deploy_with_override(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            &override_config,
            &extension(),
            Some(&verification),
            Some(temp.path().to_str().expect("site root")),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(result.success, "deploy failed: {:?}", result.error);
        assert!(target.join("fixture.manifest").exists());
        assert!(!target.join("stale.php").exists());
        assert!(!staging.join("fixture.zip").exists());
    }

    #[test]
    fn test_archive_install_policy_finds_target_basename_manifest_after_nested_file() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("fixture.zip");
        let staging = temp.path().join("staging");
        let target = temp.path().join("packages/fixture");

        write_zip(
            &artifact,
            &[
                ("fixture/inc/helpers.manifest", "Helper Name: Fixture\n"),
                ("fixture/fixture.manifest", "Package Name: Fixture\n"),
            ],
        );

        let policy = package_archive_policy(staging.to_string_lossy().to_string());
        let override_config = archive_install_override(&policy);
        let verification = archive_install_verification(&policy).expect("verification");

        let result = deploy_with_override(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            &override_config,
            &extension(),
            Some(&verification),
            Some(temp.path().to_str().expect("site root")),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(result.success, "deploy failed: {:?}", result.error);
        assert!(target.join("fixture.manifest").exists());
        assert!(target.join("inc/helpers.manifest").exists());
    }

    #[test]
    fn test_archive_install_policy_verifies_required_file_header() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("fixture-bundle.zip");
        let staging = temp.path().join("staging");
        let target = temp.path().join("bundles/fixture-bundle");

        write_zip(
            &artifact,
            &[(
                "fixture-bundle/bundle.manifest",
                "Bundle Name: Fixture Bundle\n",
            )],
        );

        let policy = bundle_archive_policy(staging.to_string_lossy().to_string());
        let override_config = archive_install_override(&policy);
        let verification = archive_install_verification(&policy).expect("verification");

        let result = deploy_with_override(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            &override_config,
            &extension(),
            Some(&verification),
            Some(temp.path().to_str().expect("site root")),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(result.success, "deploy failed: {:?}", result.error);
        assert!(target.join("bundle.manifest").exists());
    }

    #[test]
    fn test_archive_install_policy_rejects_wrong_root() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("fixture.zip");
        let staging = temp.path().join("staging");
        let target = temp.path().join("packages/fixture");

        write_zip(
            &artifact,
            &[("other/fixture.manifest", "Package Name: Fixture\n")],
        );

        let policy = package_archive_policy(staging.to_string_lossy().to_string());
        let override_config = archive_install_override(&policy);
        let verification = archive_install_verification(&policy).expect("verification");

        let result = deploy_with_override(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            &override_config,
            &extension(),
            Some(&verification),
            Some(temp.path().to_str().expect("site root")),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("does not match target basename"),
            "unexpected error: {:?}",
            result.error
        );
    }

    fn nested_tree_archive_policy(staging_path: String) -> DeployArchiveInstallPolicy {
        DeployArchiveInstallPolicy {
            path_pattern: "/components/".to_string(),
            staging_path,
            root_must_match_target_basename: false,
            required_header: None,
            skip_permissions_fix: true,
        }
    }

    fn list_adjacent_install_temp_dirs(parent: &Path, basename: &str) -> Vec<std::path::PathBuf> {
        let mut leaked = Vec::new();
        let entries = match fs::read_dir(parent) {
            Ok(entries) => entries,
            Err(_) => return leaked,
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Adjacent extraction temp dirs (.homeboy-install.*) and any
            // leftover backup dirs (<basename>.homeboy-bak.*) must not survive
            // a deploy. Both would indicate the same-filesystem swap leaked.
            if name.starts_with(".homeboy-install.")
                || name.starts_with(&format!("{basename}.homeboy-bak."))
            {
                leaked.push(entry.path());
            }
        }
        leaked
    }

    // Issue #3027: replacing an existing target directory that contains a deep
    // nested package tree (node_modules-style) must succeed without relying on
    // a cross-device `mv` that preserves permissions per file. This is a
    // generic, runtime-agnostic directory-tree replace — no domain semantics.
    #[test]
    fn test_archive_install_replaces_target_with_deep_nested_tree() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("component.zip");
        let staging = temp.path().join("staging");
        let parent = temp.path().join("components");
        let target = parent.join("component");

        // Existing install with a deep nested package tree, mirroring the
        // vendor/<pkg>/packages/<sub>/node_modules layout from the bug report.
        let deep = target.join("vendor/inner-cli/packages/runtime-core/node_modules/dep/lib");
        fs::create_dir_all(&deep).expect("deep nested tree");
        fs::write(deep.join("index.js"), "module.exports = {};").expect("nested file");
        fs::write(target.join("stale.txt"), "stale").expect("stale file");

        write_zip(
            &artifact,
            &[
                ("component/manifest.txt", "fresh\n"),
                ("component/vendor/pkg/node_modules/x/y.js", "ok"),
            ],
        );

        let policy = nested_tree_archive_policy(staging.to_string_lossy().to_string());
        let override_config = archive_install_override(&policy);

        let result = deploy_with_override(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            &override_config,
            &extension(),
            None,
            Some(temp.path().to_str().expect("site root")),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(result.success, "deploy failed: {:?}", result.error);
        assert!(target.join("manifest.txt").exists());
        assert!(target.join("vendor/pkg/node_modules/x/y.js").exists());
        // The old nested tree and stale files must be gone after replace.
        assert!(!target.join("stale.txt").exists());
        assert!(!deep.join("index.js").exists());
        // No adjacent extraction temp dir or backup dir may leak.
        assert!(
            list_adjacent_install_temp_dirs(&parent, "component").is_empty(),
            "adjacent install/backup dirs leaked: {:?}",
            list_adjacent_install_temp_dirs(&parent, "component")
        );
    }

    // A symlink inside the existing tree must not break the same-filesystem
    // backup rename — rename(2) moves the link itself, never dereferences it.
    #[cfg(unix)]
    #[test]
    fn test_archive_install_replaces_target_containing_symlink() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("component.zip");
        let staging = temp.path().join("staging");
        let parent = temp.path().join("components");
        let target = parent.join("component");

        let nested = target.join("node_modules/.bin");
        fs::create_dir_all(&nested).expect("nested dir");
        fs::write(target.join("node_modules/real.js"), "real").expect("real file");
        std::os::unix::fs::symlink("../real.js", nested.join("link.js")).expect("symlink");

        write_zip(&artifact, &[("component/manifest.txt", "fresh\n")]);

        let policy = nested_tree_archive_policy(staging.to_string_lossy().to_string());
        let override_config = archive_install_override(&policy);

        let result = deploy_with_override(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            &override_config,
            &extension(),
            None,
            Some(temp.path().to_str().expect("site root")),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(result.success, "deploy failed: {:?}", result.error);
        assert!(target.join("manifest.txt").exists());
        assert!(!target.join("node_modules").exists());
        assert!(list_adjacent_install_temp_dirs(&parent, "component").is_empty());
    }

    // If the install step aborts before the new tree is in place, the original
    // directory (including its nested tree) must remain intact so a failed
    // deploy never leaves the target without its contents.
    #[test]
    fn test_archive_install_restores_backup_on_failure() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("component.zip");
        let staging = temp.path().join("staging");
        let parent = temp.path().join("components");
        let target = parent.join("component");

        let nested = target.join("vendor/pkg/node_modules/dep");
        fs::create_dir_all(&nested).expect("nested dir");
        fs::write(nested.join("keep.js"), "keep").expect("keep file");
        fs::write(target.join("manifest.txt"), "v1\n").expect("existing marker");

        // Ship a malformed archive whose only top-level entry is a *file*, not
        // a directory. `zip_root` resolves to that file name, so the extracted
        // path "$staged/$zip_root" is not a directory and the install aborts at
        // the "expected directory ... not found in archive" guard. This guard
        // intentionally fires *before* the existing target is touched, so the
        // original install (including its deep nested tree) must remain fully
        // intact and no staging/backup dirs may leak.
        write_zip(&artifact, &[("manifest.txt", "v2\n")]);

        let policy = nested_tree_archive_policy(staging.to_string_lossy().to_string());
        let override_config = archive_install_override(&policy);

        let result = deploy_with_override(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            &override_config,
            &extension(),
            None,
            Some(temp.path().to_str().expect("site root")),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(!result.success, "expected install to fail and roll back");
        // Original install fully preserved, nested tree intact.
        assert_eq!(
            fs::read_to_string(target.join("manifest.txt")).expect("preserved marker"),
            "v1\n"
        );
        assert!(nested.join("keep.js").exists(), "nested tree not preserved");
        // No backup or extraction temp dir may survive the failed install.
        assert!(
            list_adjacent_install_temp_dirs(&parent, "component").is_empty(),
            "backup/temp dirs leaked after failure: {:?}",
            list_adjacent_install_temp_dirs(&parent, "component")
        );
    }

    #[test]
    fn test_deploy_override_template_vars_include_target_adjacent_temp_pattern() {
        let vars = deploy_override_template_vars(
            "artifact.zip",
            "/tmp/homeboy-staging/artifact.zip",
            "/srv/htdocs/wp-content/plugins/sample-plugin",
            Some("/srv/htdocs"),
            "wp",
            Some("example.com"),
        );

        assert_eq!(
            vars.get(TemplateVars::TARGET_PARENT_DIR)
                .map(String::as_str),
            Some("/srv/htdocs/wp-content/plugins")
        );
        assert_eq!(
            vars.get(TemplateVars::TARGET_ADJACENT_TEMP_PATTERN)
                .map(String::as_str),
            Some("/srv/htdocs/wp-content/plugins/.homeboy-install.XXXXXX")
        );
        assert_eq!(
            vars.get(TemplateVars::TARGET_BASENAME).map(String::as_str),
            Some("sample-plugin")
        );
        assert_eq!(
            render_map(
                "mktemp -d {{targetAdjacentTempPattern}} && cp {{stagingArtifact}} {{targetDir}}/installed.zip",
                &vars,
            ),
            "mktemp -d /srv/htdocs/wp-content/plugins/.homeboy-install.XXXXXX && cp /tmp/homeboy-staging/artifact.zip /srv/htdocs/wp-content/plugins/sample-plugin/installed.zip"
        );
    }

    // The generated archive-install command must stage extraction into an
    // adjacent temp dir and swap the target into place with same-filesystem
    // renames. It must never `rm -rf` the live target before the new tree is
    // staged (that opens a window where the target does not exist).
    #[test]
    fn archive_install_command_stages_adjacent_and_swaps_atomically() {
        let policy = package_archive_policy("/tmp/staging".to_string());
        let cmd = archive_install_override(&policy).install_command;

        assert!(
            cmd.contains("mktemp -d \"{{targetAdjacentTempPattern}}\""),
            "install must extract into an adjacent temp dir: {cmd}"
        );
        assert!(
            cmd.contains("unzip -oq \"{{stagingArtifact}}\" -d \"$staged\""),
            "install must unpack into the adjacent temp dir, not over the target: {cmd}"
        );
        assert!(
            cmd.contains("mv \"$extracted\" \"$target\""),
            "install must rename the staged tree into place: {cmd}"
        );
        assert!(
            !cmd.contains("rm -rf \"{{targetDir}}\""),
            "install must not rm -rf the live target before staging the new tree: {cmd}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn target_is_symlink_detects_symlink_vs_real_dir() {
        let temp = tempfile::tempdir().expect("temp dir");
        let real = temp.path().join("real");
        fs::create_dir(&real).expect("real dir");
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        assert!(target_is_symlink(
            &local_client(),
            link.to_str().expect("link path")
        ));
        assert!(!target_is_symlink(
            &local_client(),
            real.to_str().expect("real path")
        ));
        assert!(!target_is_symlink(
            &local_client(),
            temp.path().join("missing").to_str().expect("missing path")
        ));
        assert!(!target_is_symlink(&local_client(), ""));
    }

    // A dev symlink at the deploy target must be replaced by a real directory
    // atomically, without writing *through* the symlink into the external
    // checkout it points at. This is the exact footgun from issue #6867.
    #[cfg(unix)]
    #[test]
    fn test_archive_install_replaces_symlink_target_with_real_dir() {
        let temp = tempfile::tempdir().expect("temp dir");
        let artifact = temp.path().join("fixture.zip");
        let staging = temp.path().join("staging");
        let parent = temp.path().join("packages");
        fs::create_dir_all(&parent).expect("parent dir");
        let target = parent.join("fixture");

        // A dev workflow symlinked an external checkout into the deploy location.
        let workspace = temp.path().join("workspace-fixture");
        fs::create_dir_all(&workspace).expect("workspace dir");
        fs::write(workspace.join("workspace-only.php"), "workspace").expect("workspace file");
        std::os::unix::fs::symlink(&workspace, &target).expect("symlink target");

        write_zip(
            &artifact,
            &[("fixture/fixture.manifest", "Package Name: Fixture\n")],
        );

        let policy = package_archive_policy(staging.to_string_lossy().to_string());
        let override_config = archive_install_override(&policy);
        let verification = archive_install_verification(&policy).expect("verification");

        let result = deploy_with_override(
            &local_client(),
            &artifact,
            target.to_str().expect("target path"),
            &override_config,
            &extension(),
            Some(&verification),
            Some(temp.path().to_str().expect("site root")),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(result.success, "deploy failed: {:?}", result.error);
        // Target is now a real directory, not a symlink.
        assert!(
            !fs::symlink_metadata(&target)
                .expect("target metadata")
                .file_type()
                .is_symlink(),
            "target should be a real directory after install"
        );
        assert!(target.join("fixture.manifest").exists());
        // The install must not have written through the old symlink into the
        // external checkout it pointed at.
        assert!(
            workspace.join("workspace-only.php").exists(),
            "external checkout must be preserved"
        );
        assert!(
            !workspace.join("fixture.manifest").exists(),
            "install must not write through the symlink into the external checkout"
        );
        // No adjacent extraction temp dir or backup dir may leak.
        assert!(
            list_adjacent_install_temp_dirs(&parent, "fixture").is_empty(),
            "adjacent install/backup dirs leaked: {:?}",
            list_adjacent_install_temp_dirs(&parent, "fixture")
        );
    }
}
