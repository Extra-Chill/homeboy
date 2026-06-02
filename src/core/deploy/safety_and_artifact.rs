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

            // Step 2b: Flatten an accidental single top-level directory.
            //
            // Build archives commonly contain a single top-level directory named
            // after the component. When the component's remote_path already points
            // at that directory, extracting in place produces a double-nested layout
            // (`.../component/component/...`) that runtimes cannot load. Detect that
            // signature and lift the inner directory's contents up one level so the
            // artifact lands flat.
            if let Some(result) = flatten_double_nested_dir(ssh_client, remote_path)? {
                return Ok(result);
            }

            // Step 2c: Fail loudly if the layout is still double-nested.
            //
            // This is independent of the optional user-configured `verification`
            // step below. A false success on a broken layout is the worst symptom:
            // it silently breaks the install while reporting `success: true`. If the
            // double-nest directory still exists after the flatten attempt, refuse to
            // report success.
            if let Some(result) = ensure_not_double_nested(ssh_client, remote_path) {
                return Ok(result);
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

/// Return the final path segment of `remote_path` (its basename), if any.
///
/// For example, `/srv/app/components/example` -> `example`.
fn remote_basename(remote_path: &str) -> Option<&str> {
    remote_path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .filter(|segment| !segment.is_empty())
}

/// Detect and repair the classic double-nested layout produced when a build ZIP
/// has a single top-level directory equal to the deploy target's own basename.
///
/// The signature we repair is: after extraction, `remote_path` contains exactly
/// one entry, and that entry is a directory whose name equals `basename(remote_path)`
/// (e.g. `.../component/component/`). When that holds, the inner
/// directory's contents (including dotfiles) are lifted up into `remote_path` and
/// the now-empty inner directory is removed.
///
/// Returns `Ok(Some(failure))` only if the flatten was attempted but failed, so the
/// caller can short-circuit and report failure. Returns `Ok(None)` when there was
/// nothing to flatten or the flatten succeeded.
fn flatten_double_nested_dir(
    ssh_client: &SshClient,
    remote_path: &str,
) -> Result<Option<DeployResult>> {
    let Some(basename) = remote_basename(remote_path) else {
        return Ok(None);
    };

    let normalized = remote_path.trim_end_matches('/');
    let nested_dir = format!("{}/{}", normalized, basename);

    // Only flatten when the nested directory is the *sole* entry in remote_path.
    // This avoids clobbering a legitimate same-named subdirectory that lives
    // alongside other top-level files (which would not be a double-nest artifact).
    //
    // `entries` counts the non-hidden + hidden top-level entries; we require it to be
    // exactly the nested directory and nothing else.
    let detect_cmd = format!(
        "test -d {nested} && \
         [ \"$(cd {target} && find . -mindepth 1 -maxdepth 1 | wc -l | tr -d '[:space:]')\" = \"1\" ] && \
         [ \"$(cd {target} && ls -A)\" = {basename_arg} ] && echo NESTED || echo OK",
        nested = shell::quote_path(&nested_dir),
        target = shell::quote_path(normalized),
        basename_arg = shell::quote_arg(basename),
    );
    let detect_output = ssh_client.execute(&detect_cmd);
    if !detect_output.success {
        // Detection is best-effort; if we cannot determine the layout, let the
        // mandatory sanity check below decide.
        return Ok(None);
    }
    if detect_output.stdout.trim() != "NESTED" {
        return Ok(None);
    }

    log_status!(
        "deploy",
        "Detected double-nested directory '{}'; flattening into '{}'",
        nested_dir,
        normalized
    );

    // Move the inner directory aside, then lift its contents (including dotfiles)
    // up into remote_path, then remove the now-empty staging directory. Using a
    // temp staging name avoids the "cannot move a directory into itself" problem
    // when the inner dir shares the basename with its parent.
    let staging = format!("{}/.artifact-flatten-staging", normalized);
    let target_dir = format!("{}/", normalized);
    let flatten_cmd = format!(
        "cd {target} && rm -rf {staging} && mv {nested} {staging} && \
         find {staging} -mindepth 1 -maxdepth 1 -exec mv {{}} {target_dir} \\; && \
         rmdir {staging}",
        target = shell::quote_path(normalized),
        target_dir = shell::quote_path(&target_dir),
        nested = shell::quote_path(&nested_dir),
        staging = shell::quote_path(&staging),
    );
    let flatten_output = ssh_client.execute(&flatten_cmd);
    if !flatten_output.success {
        let error_detail = if flatten_output.stderr.is_empty() {
            flatten_output.stdout.clone()
        } else {
            flatten_output.stderr.clone()
        };
        return Ok(Some(DeployResult::failure(
            flatten_output.exit_code,
            format!(
                "Failed to flatten double-nested directory '{}' (exit {}): {}",
                nested_dir, flatten_output.exit_code, error_detail
            ),
        )));
    }

    Ok(None)
}

/// Mandatory post-extract sanity check: fail the deploy if the artifact landed
/// double-nested (`remote_path/<basename(remote_path)>/` still exists).
///
/// This guards against silently reporting `success: true` while the install is
/// broken on disk. Returns `Some(failure)` when the broken layout is detected.
fn ensure_not_double_nested(ssh_client: &SshClient, remote_path: &str) -> Option<DeployResult> {
    let basename = remote_basename(remote_path)?;
    let normalized = remote_path.trim_end_matches('/');
    let nested_dir = format!("{}/{}", normalized, basename);

    let check_cmd = format!(
        "test -d {} && echo NESTED || echo OK",
        shell::quote_path(&nested_dir)
    );
    let check_output = ssh_client.execute(&check_cmd);
    if check_output.stdout.trim() == "NESTED" {
        return Some(DeployResult::failure(
            1,
            format!(
                "Deploy produced a double-nested layout: '{nested}' exists, so the artifact \
                 landed at '{nested}/...' instead of '{target}/...'. Runtimes generally cannot \
                 load a component nested one level too deep. Refusing to \
                 report success. Set the component's remote_path to the parent directory, or \
                 adjust the build so the archive does not contain a redundant top-level '{base}/' \
                 directory.",
                nested = nested_dir,
                target = normalized,
                base = basename,
            ),
        ));
    }
    None
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
    use super::{
        deploy_artifact, ensure_not_double_nested, flatten_double_nested_dir, remote_basename,
        render_extract_command, DANGEROUS_PATH_SUFFIXES,
    };
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

    #[test]
    fn test_remote_basename_extracts_final_segment() {
        assert_eq!(
            remote_basename("wp-content/plugins/extrachill-users"),
            Some("extrachill-users")
        );
        assert_eq!(
            remote_basename("wp-content/plugins/extrachill-users/"),
            Some("extrachill-users")
        );
        assert_eq!(remote_basename("plugin"), Some("plugin"));
        assert_eq!(remote_basename(""), None);
        assert_eq!(remote_basename("/"), None);
    }

    /// Build a zip whose sole top-level entry is a directory `<name>/` containing
    /// the given relative files. Returns the path to the created archive.
    #[cfg(unix)]
    fn make_zip_with_top_level_dir(
        temp: &std::path::Path,
        archive_name: &str,
        top_dir: &str,
        files: &[(&str, &str)],
    ) -> std::path::PathBuf {
        let staging = temp.join("zip-staging");
        let root = staging.join(top_dir);
        fs::create_dir_all(&root).expect("staging root");
        for (rel, contents) in files {
            let path = root.join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("file parent");
            }
            fs::write(&path, contents).expect("staged file");
        }

        let archive = temp.join(archive_name);
        let status = std::process::Command::new("zip")
            .args(["-q", "-r", archive.to_str().expect("archive path"), top_dir])
            .current_dir(&staging)
            .status()
            .expect("run zip");
        assert!(status.success(), "zip command failed");
        archive
    }

    /// End-to-end: a build ZIP with a top-level dir equal to the target basename,
    /// deployed with the real-world `unzip -o {artifact} && rm {artifact}` command,
    /// must land FLAT (no double-nesting) and report success.
    #[test]
    #[cfg(unix)]
    fn test_deploy_artifact_flattens_double_nested_plugin_zip() {
        let temp = tempfile::tempdir().expect("temp dir");
        let plugin = "extrachill-users";
        let archive = make_zip_with_top_level_dir(
            temp.path(),
            "build.zip",
            plugin,
            &[
                ("extrachill-users.php", "<?php // plugin main"),
                ("src/loader.php", "<?php // loader"),
                (".gitkeep", ""),
            ],
        );

        // remote_path points AT the plugin dir (the prod misconfiguration that
        // triggers double-nesting).
        let target = temp.path().join("wp-content/plugins").join(plugin);

        let result = deploy_artifact(
            &local_client(),
            &archive,
            target.to_str().expect("target path"),
            Some("unzip -o {artifact} && rm {artifact}"),
            None,
            None,
        )
        .expect("deploy result");

        assert!(result.success, "deploy should succeed: {:?}", result.error);
        // Flat layout: main file directly under remote_path.
        assert!(
            target.join("extrachill-users.php").is_file(),
            "plugin main file must land flat at remote_path"
        );
        assert!(target.join("src/loader.php").is_file());
        assert!(target.join(".gitkeep").is_file());
        // No double-nesting.
        assert!(
            !target.join(plugin).exists(),
            "double-nested directory must not exist after flatten"
        );
        // Extract artifact and flatten staging cleaned up.
        assert!(!target.join(".artifact-flatten-staging").exists());
    }

    /// A normal (non-nested) archive must extract in place untouched.
    #[test]
    #[cfg(unix)]
    fn test_deploy_artifact_leaves_flat_archive_untouched() {
        let temp = tempfile::tempdir().expect("temp dir");
        let staging = temp.path().join("flat-staging");
        fs::create_dir_all(&staging).expect("flat staging");
        fs::write(staging.join("plugin.php"), "<?php // main").expect("main file");
        fs::write(staging.join("readme.txt"), "readme").expect("readme");

        let archive = temp.path().join("flat.zip");
        let status = std::process::Command::new("zip")
            .args(["-q", "-r", archive.to_str().expect("archive"), "."])
            .current_dir(&staging)
            .status()
            .expect("run zip");
        assert!(status.success());

        let target = temp.path().join("wp-content/plugins/flat-plugin");
        let result = deploy_artifact(
            &local_client(),
            &archive,
            target.to_str().expect("target path"),
            Some("unzip -o {artifact} && rm {artifact}"),
            None,
            None,
        )
        .expect("deploy result");

        assert!(result.success, "deploy should succeed: {:?}", result.error);
        assert!(target.join("plugin.php").is_file());
        assert!(target.join("readme.txt").is_file());
        // No spurious nested dir matching the target basename.
        assert!(!target.join("flat-plugin").exists());
    }

    /// The mandatory sanity check must report failure when a double-nested layout
    /// remains (e.g. an extract path the flatten heuristic could not repair).
    #[test]
    #[cfg(unix)]
    fn test_ensure_not_double_nested_detects_broken_layout() {
        let temp = tempfile::tempdir().expect("temp dir");
        let plugin = "extrachill-users";
        let target = temp.path().join(plugin);
        let nested = target.join(plugin);
        fs::create_dir_all(&nested).expect("nested dir");
        fs::write(nested.join("extrachill-users.php"), "<?php").expect("nested main");
        // Also place a sibling file so the auto-flatten heuristic would NOT trigger
        // (more than one top-level entry), but the broken layout still exists.
        fs::write(target.join("stray.txt"), "stray").expect("stray");

        let result = ensure_not_double_nested(&local_client(), target.to_str().expect("target"));
        let result = result.expect("should detect broken layout");
        assert!(!result.success);
        let error = result.error.expect("error message");
        assert!(error.contains("double-nested layout"));
        assert!(error.contains(plugin));
    }

    /// The flatten helper is a no-op (returns Ok(None)) when there is nothing nested.
    #[test]
    #[cfg(unix)]
    fn test_flatten_double_nested_dir_noop_when_flat() {
        let temp = tempfile::tempdir().expect("temp dir");
        let target = temp.path().join("flat-plugin");
        fs::create_dir_all(&target).expect("target");
        fs::write(target.join("plugin.php"), "<?php").expect("main");

        let result = flatten_double_nested_dir(&local_client(), target.to_str().expect("target"))
            .expect("ok");
        assert!(
            result.is_none(),
            "flatten should be a no-op on a flat layout"
        );
        assert!(target.join("plugin.php").is_file());
    }

    /// The "requires an extractCommand" hint must use single-brace `{artifact}`
    /// placeholders so a copy-paste of the JSON works (render_extract_command +
    /// render_map both resolve `{artifact}` correctly).
    #[test]
    fn test_archive_without_extract_command_hint_uses_single_brace_placeholder() {
        let temp = tempfile::tempdir().expect("temp dir");
        let archive = temp.path().join("plugin.zip");
        fs::write(&archive, "zip bytes").expect("archive");
        let target = temp.path().join("wp-content/plugins/plugin");

        let result = deploy_artifact(
            &local_client(),
            &archive,
            target.to_str().expect("target"),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(!result.success);
        let error = result.error.expect("hint error");
        assert!(
            error.contains("{artifact}"),
            "hint must contain single-brace placeholder: {error}"
        );
        assert!(
            !error.contains("{{artifact}}"),
            "hint must NOT contain double-brace placeholder: {error}"
        );
    }
}
