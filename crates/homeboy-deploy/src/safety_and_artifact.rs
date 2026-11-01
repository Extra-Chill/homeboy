use std::collections::HashMap;
use std::path::Path;

use super::lifecycle::DeployObservation;
use super::permissions;
use super::phase_clock;
use homeboy_core::component;
use homeboy_core::defaults;
use homeboy_core::engine::shell;
use homeboy_core::engine::template::{render_map, TemplateVars};
use homeboy_core::error::{Error, Result};
use homeboy_core::phase_timing::PhaseTimer;
use homeboy_core::server::SshClient;
use homeboy_extension_contract::DeployVerification;
use std::time::Instant;

use super::install_script::{self, shown, Step, StepOutput};
use super::transfer::{scp_file, upload_directory, upload_file};
use super::types::{DeployEffect, DeployResult};
use super::version_overrides::deploy_override_template_vars;
use homeboy_extension_contract::manifest_toolchain_config::DeployOverride;
use homeboy_extension_contract::ExtensionManifest;

/// Framework-neutral shared directory names that typically contain sibling components.
const DANGEROUS_PATH_SUFFIXES: &[&str] = &["/node_modules", "/vendor", "/packages", "/extensions"];

/// Deploy a component via git pull on the remote server.
pub(super) fn deploy_via_git(
    ssh_client: &SshClient,
    remote_path: &str,
    git_config: &component::GitDeployConfig,
    component_version: Option<&str>,
    observation: Option<&mut DeployObservation>,
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

    let mut timer = PhaseTimer::new();
    let subject = format!("target={remote_path}");

    // Step 1: Fetch latest. `git fetch` writes remote-tracking refs and
    // FETCH_HEAD, so record the mutation boundary before issuing it.
    if let Some(observation) = observation {
        observation.phase("transfer", true)?;
    }
    let started = Instant::now();
    homeboy_core::log_status!(
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
    phase_clock::record(&mut timer, &subject, "transfer", started);
    let started = Instant::now();

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
    homeboy_core::log_status!("deploy:git", "Checking out {}", checkout_target);
    let checkout_output = ssh_client.execute(&checkout_cmd);
    if !checkout_output.success {
        return Ok(DeployResult::failure(
            checkout_output.exit_code,
            format!("git checkout/pull failed: {}", checkout_output.stderr),
        ));
    }

    // Step 3: Run post-pull commands
    for cmd in &git_config.post_pull {
        homeboy_core::log_status!("deploy:git", "Running: {}", cmd);
        let full_cmd = format!("cd {} && {}", shell::quote_path(remote_path), cmd);
        let output = ssh_client.execute(&full_cmd);
        if !output.success {
            return Ok(DeployResult::failure(
                output.exit_code,
                format!("post-pull command failed ({}): {}", cmd, output.stderr),
            ));
        }
    }

    phase_clock::record(&mut timer, &subject, "install", started);
    homeboy_core::log_status!("deploy:git", "Deploy complete for {}", remote_path);
    Ok(DeployResult::success(0).with_phase_timings(timer))
}

/// How an uploaded artifact is installed at its target.
pub(super) enum ArtifactInstall<'a> {
    /// Upload in place: archives go into the target directory and run the
    /// component's extract command; other files and directories land as-is.
    Extract(Option<&'a str>),
    /// An extension-defined override (e.g. an archive install policy): upload
    /// to its staging path and run its install command.
    Override(OverrideInstall<'a>),
}

pub(super) struct OverrideInstall<'a> {
    pub config: &'a DeployOverride,
    pub extension: &'a ExtensionManifest,
    pub site_root: Option<&'a str>,
    pub domain: Option<&'a str>,
    pub cli_path: Option<&'a str>,
}

/// The single artifact install path: upload once, then run every install,
/// permission, verification, and cleanup step as one remote script.
pub(super) fn deploy_artifact(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
    install: &ArtifactInstall<'_>,
    verification: Option<&DeployVerification>,
    remote_owner: Option<&str>,
    mut observation: Option<&mut DeployObservation>,
) -> Result<DeployResult> {
    let mut timer = PhaseTimer::new();
    let subject = format!("target={remote_path}");
    if let Some(observation) = observation.as_deref_mut() {
        observation.phase("transfer", true)?;
    }
    let started = Instant::now();
    let mut vars = HashMap::from([(
        TemplateVars::TARGET_DIR.to_string(),
        remote_path.to_string(),
    )]);
    let mut steps = Vec::new();
    let mut fix_permissions = true;
    let mut installs = true;
    let mut cleanup = None;

    let uploaded_artifact_path = match install {
        ArtifactInstall::Extract(_) if local_path.is_dir() => {
            // Directory artifacts bypass extraction but still get their
            // permissions normalized instead of keeping the build user's.
            installs = false;
            let result = upload_directory(ssh_client, local_path, remote_path)?;
            if !result.success {
                return Ok(result);
            }
            None
        }
        ArtifactInstall::Extract(extract_command) => {
            let local_filename = artifact_file_name(local_path)?;
            let artifact_filename = match extract_command {
                Some(_) => {
                    if let Some(failure) = refuse_dangerous_clean(remote_path) {
                        return Ok(failure);
                    }
                    // Archives upload into the target directory with a prefix, then extract.
                    let prefix = defaults::load_defaults().deploy.artifact_prefix;
                    format!("{}{}", prefix, local_filename)
                }
                None if is_archive(local_path) => {
                    return Ok(DeployResult::failure(
                        1,
                        format!(
                            "Archive artifact '{}' requires an extractCommand. \
                             Add one with: homeboy component set <id> --json '{{\"extract_command\": \"unzip -o {{{{artifact}}}} && rm {{{{artifact}}}}\"}}'",
                            local_path.display()
                        ),
                    ));
                }
                // Other files upload to their own path, replaced atomically via temp + mv.
                None => local_filename.to_string(),
            };
            let upload_path = format!("{}/{}", remote_path, artifact_filename);
            if let Some(failure) =
                upload_into(ssh_client, local_path, remote_path, &upload_path, false)?
            {
                return Ok(failure);
            }
            match extract_command {
                Some(cmd_template) => {
                    steps.extend(extract_steps(remote_path, artifact_filename, cmd_template));
                }
                None => {
                    fix_permissions = false;
                    installs = false;
                }
            }
            vars.insert("stagingArtifact".to_string(), upload_path.clone());
            Some(upload_path)
        }
        ArtifactInstall::Override(install) => {
            let artifact_filename = artifact_file_name(local_path)?;
            let staging_path = &install.config.staging_path;
            let staging_artifact = format!("{}/{}", staging_path, artifact_filename);
            homeboy_core::log_status!(
                "deploy",
                "Using extension deploy override: {}",
                install.extension.id
            );
            if let Some(failure) = upload_into(
                ssh_client,
                local_path,
                staging_path,
                &staging_artifact,
                true,
            )? {
                return Ok(failure);
            }
            // Resolution order: component/project cli_path override → extension default → "wp"
            let default_cli = install.extension.cli.as_ref();
            let cli_path = install
                .cli_path
                .or_else(|| default_cli.and_then(|c| c.default_cli_path.as_deref()))
                .unwrap_or("wp");
            vars = deploy_override_template_vars(
                artifact_filename,
                &staging_artifact,
                remote_path,
                install.site_root,
                cli_path,
                install.domain,
            );
            steps.push(symlink_notice_step(remote_path));
            let install_cmd = render_map(&install.config.install_command, &vars);
            homeboy_core::log_status!("deploy", "Running install command: {}", install_cmd);
            steps.push(Step::new(install_cmd, |output| {
                Ok(failure_with_detail("Install command failed", output))
            }));
            fix_permissions = !install.config.skip_permissions_fix;
            cleanup = install.config.cleanup_command.as_ref();
            Some(staging_artifact)
        }
    };

    phase_clock::record(&mut timer, &subject, "transfer", started);

    if fix_permissions {
        homeboy_core::log_status!("deploy", "Fixing file permissions");
        steps.extend(permissions::fix_deployed_permissions_steps(
            remote_path,
            remote_owner,
        ));
    }
    let verify = verification.and_then(|v| v.verify_command.as_ref().map(|cmd| (v, cmd)));
    if let Some((v, template)) = verify {
        let error_msg = v
            .verify_error_message
            .as_ref()
            .map(|msg| render_map(msg, &vars))
            .unwrap_or_else(|| format!("Deploy verification failed for {}", remote_path));
        steps.push(Step::requiring_output(
            &render_map(template, &vars),
            move |_| Ok(DeployResult::failure(1, error_msg.clone())),
        ));
    }
    // Cleanup runs after verification so verifiers can compare the installed
    // files against the exact uploaded payload.
    if let Some(cleanup) = cleanup {
        let cleanup_cmd = render_map(cleanup, &vars);
        homeboy_core::log_status!("deploy", "Running cleanup: {}", cleanup_cmd);
        steps.push(Step::best_effort(cleanup_cmd));
    }

    if installs {
        if let Some(observation) = observation.as_deref_mut() {
            observation.phase("extract", true)?;
        }
    }
    // Install, permissions, verification, and cleanup run as one remote
    // script, so they are timed as a single `install` phase.
    let failure = if steps.is_empty() {
        None
    } else {
        phase_clock::time(&mut timer, &subject, "install", || {
            install_script::run(ssh_client, &steps)
        })?
    };
    if let (Some(observation), Some(_)) = (observation, verification) {
        observation.phase("verify", true)?;
    }
    if let Some(failure) = failure {
        return Ok(failure);
    }
    Ok(DeployResult::success(0)
        .with_effect(DeployEffect {
            remote_path: remote_path.to_string(),
            artifact_path: uploaded_artifact_path,
            verified: verify.is_some(),
        })
        .with_phase_timings(timer))
}

fn is_archive(local_path: &Path) -> bool {
    local_path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| matches!(ext, "zip" | "tar" | "gz" | "tgz"))
}

fn artifact_file_name(local_path: &Path) -> Result<&str> {
    local_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "buildArtifact",
                "Build artifact path must include a file name",
                Some(local_path.display().to_string()),
                None,
            )
        })
}

/// Create `dir` and upload the artifact to `upload_path` inside it. Override
/// staging uploads in place; target uploads replace atomically via temp + mv.
fn upload_into(
    ssh_client: &SshClient,
    local_path: &Path,
    dir: &str,
    upload_path: &str,
    staging: bool,
) -> Result<Option<DeployResult>> {
    let (created, failed) = if staging {
        ("staging directory", "staging directory")
    } else {
        ("directory", "remote directory")
    };
    homeboy_core::log_status!("deploy", "Creating {}: {}", created, dir);
    let mkdir = ssh_client.execute(&format!("mkdir -p {}", shell::quote_path(dir)));
    if !mkdir.success {
        return Ok(Some(DeployResult::failure(
            mkdir.exit_code,
            format!("Failed to create {}: {}", failed, mkdir.stderr),
        )));
    }
    let upload = if staging {
        scp_file(ssh_client, local_path, upload_path)?
    } else {
        upload_file(ssh_client, local_path, upload_path)?
    };
    Ok((!upload.success).then_some(upload))
}

fn failure_with_detail(what: &str, output: &StepOutput) -> DeployResult {
    DeployResult::failure(
        output.exit_code,
        format!("{what} (exit {}): {}", output.exit_code, output.detail()),
    )
}

/// Defense-in-depth: refuse to clean known shared parent directories. The
/// upstream validate_deploy_target() should already catch this, but the
/// extract steps run `rm -rf`, so guard again.
fn refuse_dangerous_clean(remote_path: &str) -> Option<DeployResult> {
    let normalized_remote = remote_path.trim_end_matches('/');
    DANGEROUS_PATH_SUFFIXES
        .iter()
        .any(|suffix| normalized_remote.ends_with(suffix))
        .then(|| {
            DeployResult::failure(
                1,
                format!(
                    "Refusing to clean '{}' — it is a shared parent directory. \
                     This would delete sibling components. Fix the component's remote_path.",
                    remote_path
                ),
            )
        })
}

fn extract_steps(remote_path: &str, artifact_filename: String, cmd_template: &str) -> Vec<Step> {
    // Clean the target directory before extraction to prevent stale files.
    // This handles directory renames (e.g. blocks/ → Blocks/) where the old
    // casing would persist because unzip merges into existing directories.
    // We remove everything except the uploaded artifact itself.
    let clean_cmd = format!(
        "cd {} && find . -mindepth 1 -maxdepth 1 ! -name {} -exec rm -rf {{}} +",
        shell::quote_path(remote_path),
        shell::quote_arg(&artifact_filename),
    );
    homeboy_core::log_status!("deploy", "Cleaning target directory before extraction");
    let vars = HashMap::from([
        ("artifact".to_string(), artifact_filename),
        ("targetDir".to_string(), remote_path.to_string()),
    ]);
    let rendered_cmd = render_extract_command(cmd_template, &vars);
    let extract_cmd = format!("cd {} && {}", shell::quote_path(remote_path), rendered_cmd);
    homeboy_core::log_status!("deploy", "Extracting: {}", rendered_cmd);

    let mut steps = vec![
        Step::new(clean_cmd, |output| {
            Ok(failure_with_detail(
                "Failed to clean target directory before extraction",
                output,
            ))
        }),
        Step::new(extract_cmd, |output| {
            Ok(failure_with_detail("Extract command failed", output))
        }),
    ];
    steps.extend(flatten_double_nested_step(remote_path));
    steps.extend(ensure_not_double_nested_step(remote_path));
    steps
}

/// Warn when the target is a symlink: the install replaces it with a real
/// directory, which changes the target's resolved path. A dev/rig workflow may
/// symlink an external checkout into the deploy location, and an environment
/// caching the old resolved path may need a refresh. Generic: no assumptions
/// about the target runtime or its caching behavior.
fn symlink_notice_step(remote_path: &str) -> Step {
    Step::best_effort(format!(
        "if [ -L {} ]; then echo \"{}⚠ Deploy target {} is currently a symlink; the install will replace it with a real directory. Any environment that caches the symlink's resolved path may need to be refreshed or recycled to pick up the new location.\"; fi",
        shell::quote_path(remote_path),
        install_script::LOG_MARKER,
        shown(remote_path),
    ))
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

/// Repair the classic double-nested layout produced when a build ZIP has a
/// single top-level directory equal to the deploy target's own basename.
///
/// Build archives commonly contain a single top-level directory named after
/// the component. When the component's remote_path already points at that
/// directory, extracting in place produces `.../component/component/...`,
/// which runtimes cannot load. When `remote_path` contains exactly one entry
/// and it is a directory named `basename(remote_path)`, the inner directory's
/// contents (including dotfiles) are lifted up and the inner directory removed.
/// A same-named subdirectory alongside other entries is left alone.
fn flatten_double_nested_step(remote_path: &str) -> Option<Step> {
    let basename = remote_basename(remote_path)?;
    let normalized = remote_path.trim_end_matches('/');
    let nested_dir = format!("{}/{}", normalized, basename);
    let target = shell::quote_path(normalized);

    // Move the inner directory aside, then lift its contents up into
    // remote_path, then remove the now-empty staging directory. A temp
    // staging name avoids "cannot move a directory into itself" when the
    // inner dir shares the basename with its parent. Detection is
    // best-effort: if it cannot run, the mandatory layout check decides.
    let staging = ".artifact-flatten-staging";
    let command = format!(
        "if test -d {nested} && \
         [ \"$(cd {target} && find . -mindepth 1 -maxdepth 1 | wc -l | tr -d '[:space:]')\" = \"1\" ] && \
         [ \"$(cd {target} && find . -mindepth 1 -maxdepth 1 -exec basename {{}} \\;)\" = {basename_arg} ]; then \
         echo \"{log}Detected double-nested directory '{shown_nested}'; flattening into '{shown_target}'\" && \
         cd {target} && rm -rf {staging} && mv {inner} {staging} && \
         find {staging} -mindepth 1 -maxdepth 1 -exec mv {{}} . \\; && \
         rmdir {staging}; fi",
        nested = shell::quote_path(&nested_dir),
        basename_arg = shell::quote_arg(basename),
        inner = shell::quote_path(basename),
        staging = shell::quote_path(staging),
        log = install_script::LOG_MARKER,
        shown_nested = shown(&nested_dir),
        shown_target = shown(normalized),
    );
    Some(Step::new(command, move |output| {
        Ok(DeployResult::failure(
            output.exit_code,
            format!(
                "Failed to flatten double-nested directory '{}' (exit {}): {}",
                nested_dir,
                output.exit_code,
                output.detail()
            ),
        ))
    }))
}

/// Mandatory post-extract sanity check: fail the deploy if the artifact landed
/// double-nested (`remote_path/<basename(remote_path)>/` still exists).
///
/// This is independent of the optional user-configured verification. A false
/// success on a broken layout is the worst symptom: it silently breaks the
/// install while reporting `success: true`.
fn ensure_not_double_nested_step(remote_path: &str) -> Option<Step> {
    let basename = remote_basename(remote_path)?.to_string();
    let normalized = remote_path.trim_end_matches('/').to_string();
    let nested_dir = format!("{}/{}", normalized, basename);
    let command = format!("! test -d {}", shell::quote_path(&nested_dir));
    Some(Step::new(command, move |_| {
        Ok(DeployResult::failure(
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
        ))
    }))
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
        deploy_artifact, deploy_via_git, ensure_not_double_nested_step, flatten_double_nested_step,
        install_script, remote_basename, render_extract_command, ArtifactInstall,
        DANGEROUS_PATH_SUFFIXES,
    };
    use crate::lifecycle::DeployObservation;
    use crate::test_support::local_client;
    use crate::types::DeployResult;
    use homeboy_core::observation::ObservationStore;
    use homeboy_core::server::SshClient;
    use homeboy_extension_contract::DeployVerification;
    use std::collections::HashMap;
    use std::collections::HashSet;
    use std::fs;
    #[cfg(unix)]
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn ensure_not_double_nested(ssh_client: &SshClient, remote_path: &str) -> Option<DeployResult> {
        let step = ensure_not_double_nested_step(remote_path)?;
        install_script::run(ssh_client, &[step]).expect("layout check")
    }

    fn flatten_double_nested_dir(
        ssh_client: &SshClient,
        remote_path: &str,
    ) -> homeboy_core::error::Result<Option<DeployResult>> {
        match flatten_double_nested_step(remote_path) {
            Some(step) => install_script::run(ssh_client, &[step]),
            None => Ok(None),
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
                ".homeboy-sample-plugin-events.zip".to_string(),
            ),
            (
                "targetDir".to_string(),
                "/srv/site/wp-content/plugins/sample-plugin-events".to_string(),
            ),
        ]);

        assert_eq!(
            render_extract_command("unzip -o {{artifact}} && rm {{artifact}}", &vars),
            "unzip -o .homeboy-sample-plugin-events.zip && rm .homeboy-sample-plugin-events.zip"
        );
    }

    #[test]
    fn test_deploy_via_git_keeps_framework_safety_policy_external() {
        assert!(DANGEROUS_PATH_SUFFIXES.contains(&"/vendor"));
        assert!(!DANGEROUS_PATH_SUFFIXES.contains(&"/wp-content/plugins"));
    }

    #[test]
    fn git_fetch_failure_records_remote_mutation_before_interruption() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("temp dir");
            let git_config = homeboy_core::component::GitDeployConfig {
                remote: "origin".to_string(),
                branch: "main".to_string(),
                ..Default::default()
            };
            let mut observation = DeployObservation::start("site", "HEAD").expect("admit run");
            let run_id = observation.run_id().to_string();

            let result = deploy_via_git(
                &local_client(),
                temp.path().to_str().expect("remote path"),
                &git_config,
                None,
                Some(&mut observation),
            )
            .expect("fetch failure is a deploy result");
            assert!(!result.success);
            drop(observation);

            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read run")
                .expect("run");

            assert_eq!(run.metadata_json["remote_mutation_started"], true);
            assert_eq!(run.status, "error");
            let report = ObservationStore::open_initialized()
                .expect("store")
                .phase_spans(&run_id)
                .expect("spans");
            assert_eq!(
                report
                    .span("stage.deploy.transfer")
                    .expect("transfer")
                    .status,
                homeboy_core::phase_timing::PhaseStatus::Failed
            );
        });
    }

    #[test]
    #[cfg(unix)]
    fn test_directory_artifact_normalizes_group_write_and_setgid() {
        // `deploy_artifact` resolves its permission modes through
        // `defaults::load_defaults()`, which reads the host's Homeboy config. An
        // operator-configured `permissions.remote` therefore decided whether
        // this assertion held, so the test could fail on a correct build purely
        // because of machine state (#10126).
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("temp dir");
            // BSD/macOS silently ignores setgid on a directory whose group the
            // caller is not a member of, and the test temp root inherits
            // `/tmp`'s `wheel` group there. Own the fixture tree with the
            // caller's group, as a real deploy target is, so the assertion
            // measures Homeboy's chmod rather than the host temp directory.
            std::os::unix::fs::chown(temp.path(), None, Some(unsafe { libc::getegid() }))
                .expect("own fixture group");
            let source = temp.path().join("source");
            let target = temp.path().join("plugins").join("sample-plugin");
            fs::create_dir_all(&source).expect("source dir");
            fs::create_dir_all(target.parent().expect("target parent")).expect("target parent dir");
            fs::write(source.join("plugin.php"), "<?php").expect("source file");
            fs::set_permissions(source.join("plugin.php"), fs::Permissions::from_mode(0o644))
                .expect("source permissions");

            let result = deploy_artifact(
                &local_client(),
                &source,
                target.to_str().expect("target path"),
                &ArtifactInstall::Extract(None),
                None,
                None,
                None,
            )
            .expect("deploy result");

            assert!(result.success, "{}", result.error.unwrap_or_default());
            let file_mode = fs::metadata(target.join("plugin.php"))
                .expect("target file")
                .permissions()
                .mode();
            let dir_mode = fs::metadata(&target)
                .expect("target dir")
                .permissions()
                .mode();
            assert_ne!(
                0,
                file_mode & 0o020,
                "deployed file should be group-writable"
            );
            assert_ne!(0, dir_mode & 0o020, "deployed dir should be group-writable");
            assert_ne!(
                0,
                dir_mode & 0o2000,
                "deployed dir should inherit its group"
            );
        });
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
            &ArtifactInstall::Extract(None),
            Some(&verification),
            None,
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
        homeboy_test_support::write_executable(
            &fake_find,
            "#!/bin/sh\necho cleanup denied >&2\nexit 42\n",
            0o755,
        )
        .expect("fake find");

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
            &ArtifactInstall::Extract(Some("true")),
            None,
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

    #[cfg(unix)]
    fn write_zip(path: &std::path::Path, files: &[(&str, &str)]) {
        let file = fs::File::create(path).expect("zip file");
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();
        let mut dirs = HashSet::new();

        for (name, contents) in files {
            let mut parts = name.rsplitn(2, '/');
            let _file_name = parts.next();
            if let Some(parent) = parts.next() {
                let mut dir = String::new();
                for segment in parent.split('/') {
                    if !dir.is_empty() {
                        dir.push('/');
                    }
                    dir.push_str(segment);
                    if dirs.insert(dir.clone()) {
                        zip.add_directory(format!("{dir}/"), options)
                            .expect("zip directory");
                    }
                }
            }
            zip.start_file(*name, options).expect("zip entry");
            zip.write_all(contents.as_bytes()).expect("zip contents");
        }

        zip.finish().expect("finish zip");
    }

    /// End-to-end: a build ZIP with a top-level dir equal to the target basename,
    /// deployed with the real-world `unzip -o {artifact} && rm {artifact}` command,
    /// must land FLAT (no double-nesting) and report success.
    #[test]
    #[cfg(unix)]
    fn test_deploy_artifact_flattens_double_nested_plugin_zip() {
        let temp = tempfile::tempdir().expect("temp dir");
        let plugin = "extrachill-users";
        let archive = temp.path().join("build.zip");
        write_zip(
            &archive,
            &[
                (
                    "extrachill-users/extrachill-users.php",
                    "<?php // plugin main",
                ),
                ("extrachill-users/src/loader.php", "<?php // loader"),
                ("extrachill-users/.gitkeep", ""),
            ],
        );

        // remote_path points AT the plugin dir (the prod misconfiguration that
        // triggers double-nesting).
        let target = temp.path().join("wp-content/plugins").join(plugin);

        let result = deploy_artifact(
            &local_client(),
            &archive,
            target.to_str().expect("target path"),
            &ArtifactInstall::Extract(Some("unzip -o {artifact} && rm {artifact}")),
            None,
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
        let archive = temp.path().join("flat.zip");
        write_zip(
            &archive,
            &[("plugin.php", "<?php // main"), ("readme.txt", "readme")],
        );

        let target = temp.path().join("wp-content/plugins/flat-plugin");
        let result = deploy_artifact(
            &local_client(),
            &archive,
            target.to_str().expect("target path"),
            &ArtifactInstall::Extract(Some("unzip -o {artifact} && rm {artifact}")),
            None,
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

    #[test]
    #[cfg(unix)]
    fn deploy_artifact_persists_transfer_extract_and_verify_phases() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("temp dir");
            let archive = temp.path().join("plugin.zip");
            let target = temp.path().join("target");
            write_zip(&archive, &[("plugin.php", "<?php // main")]);
            let verification = DeployVerification {
                path_pattern: target.to_string_lossy().to_string(),
                verify_command: Some(
                    "test -f {{targetDir}}/plugin.php && printf verified".to_string(),
                ),
                verify_error_message: None,
            };
            let mut observation = DeployObservation::start("site", "HEAD").expect("admit run");
            let run_id = observation.run_id().to_string();

            let result = deploy_artifact(
                &local_client(),
                &archive,
                target.to_str().expect("target path"),
                &ArtifactInstall::Extract(Some("unzip -o {artifact} && rm {artifact}")),
                Some(&verification),
                None,
                Some(&mut observation),
            )
            .expect("deploy result");

            assert!(result.success, "{}", result.error.unwrap_or_default());
            let run = ObservationStore::open_initialized()
                .expect("store")
                .get_run(&run_id)
                .expect("read run")
                .expect("run");

            assert_eq!(run.metadata_json["remote_mutation_started"], true);
        });
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

    /// The "requires an extractCommand" hint must use the canonical double-brace
    /// `{{artifact}}` template syntax so copy-pasted JSON matches configured components.
    #[test]
    fn test_archive_without_extract_command_hint_uses_double_brace_placeholder() {
        let temp = tempfile::tempdir().expect("temp dir");
        let archive = temp.path().join("plugin.zip");
        fs::write(&archive, "zip bytes").expect("archive");
        let target = temp.path().join("wp-content/plugins/plugin");

        let result = deploy_artifact(
            &local_client(),
            &archive,
            target.to_str().expect("target"),
            &ArtifactInstall::Extract(None),
            None,
            None,
            None,
        )
        .expect("deploy result");

        assert!(!result.success);
        let error = result.error.expect("hint error");
        assert!(
            error.contains("unzip -o {{artifact}} && rm {{artifact}}"),
            "hint must contain double-brace placeholder: {error}"
        );
        assert!(
            !error.contains("unzip -o {artifact} && rm {artifact}"),
            "hint must not suggest the single-brace placeholder form: {error}"
        );
    }

    /// Relative remote paths already run flatten from inside `remote_path`; the
    /// inner directory must therefore be addressed by basename only, not by
    /// prefixing `remote_path` a second time.
    #[test]
    #[cfg(unix)]
    fn test_flatten_double_nested_dir_handles_relative_remote_path() {
        let current_dir = std::env::current_dir().expect("current dir");
        let temp = tempfile::Builder::new()
            .prefix("homeboy-relative-flatten-")
            .tempdir_in(&current_dir)
            .expect("temp dir in cwd");
        let plugin = "sample-plugin";
        let target = temp.path().join("wp-content/plugins").join(plugin);
        let nested = target.join(plugin);
        fs::create_dir_all(&nested).expect("nested dir");
        fs::write(nested.join("sample-plugin.php"), "<?php").expect("plugin main");

        let relative_target = target
            .strip_prefix(&current_dir)
            .expect("target under cwd")
            .to_str()
            .expect("relative target");

        let result = flatten_double_nested_dir(&local_client(), relative_target).expect("ok");

        assert!(result.is_none(), "flatten should not fail");
        assert!(target.join("sample-plugin.php").is_file());
        assert!(!target.join(plugin).exists());
        assert!(!target.join(".artifact-flatten-staging").exists());
    }
}
