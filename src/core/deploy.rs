use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::time::SystemTime;

use crate::build;
use crate::component::{self, Component};
use crate::config;
use crate::context::{resolve_project_ssh_with_base_path, RemoteProjectContext};
use crate::defaults;
use crate::error::{Error, Result};
use crate::git;
use crate::hooks::{self, HookFailureMode};
use crate::extension::{
    self, load_all_extensions, DeployOverride, DeployVerification, ExtensionManifest,
};
use crate::permissions;
use crate::project::{self, Project};
use crate::ssh::SshClient;
use crate::utils::{self, artifact};
use crate::utils::base_path;
use crate::utils::parser;
use crate::utils::shell;
use crate::utils::template::{render_map, TemplateVars};
use crate::version;

/// Parse bulk component IDs from a JSON spec.
pub fn parse_bulk_component_ids(json_spec: &str) -> Result<Vec<String>> {
    let input = config::parse_bulk_ids(json_spec)?;
    Ok(input.component_ids)
}

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

/// Deploy a component via git pull on the remote server.
fn deploy_via_git(
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
fn deploy_artifact(
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

fn upload_directory(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    rsync_directory(ssh_client, local_path, remote_path)
}

/// Sync a local directory to the remote using rsync with --delete.
///
/// This ensures the remote directory mirrors the source exactly:
/// files removed or moved in the source are removed from the target.
/// Without --delete, stale files accumulate on the server and can
/// shadow new files (e.g. when PHP autoloader loads an old copy).
fn rsync_directory(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    // Ensure local_path ends with / so rsync copies contents, not the directory itself
    let local_str = format!("{}/", local_path.display().to_string().trim_end_matches('/'));

    // Ensure remote_path ends with /
    let remote_str = format!("{}/", remote_path.trim_end_matches('/'));

    if ssh_client.is_local {
        // Local deploy: rsync locally without SSH
        log_status!(
            "deploy",
            "Syncing directory (local rsync): {} -> {}",
            local_str,
            remote_str
        );

        let rsync_args = vec![
            "-a".to_string(),      // archive mode (recursive, preserves permissions, timestamps, etc.)
            "--delete".to_string(), // remove files on target that don't exist in source
            local_str,
            remote_str,
        ];

        let output = Command::new("rsync").args(&rsync_args).output();
        return match output {
            Ok(output) if output.status.success() => Ok(DeployResult::success(0)),
            Ok(output) => Ok(DeployResult::failure(
                output.status.code().unwrap_or(1),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )),
            Err(err) => Ok(DeployResult::failure(1, format!("rsync failed: {}", err))),
        };
    }

    // Remote deploy: rsync over SSH
    let mut rsync_args = vec![
        "-a".to_string(),
        "--delete".to_string(),
    ];

    // Build SSH command with the same options as scp
    let mut ssh_cmd_parts = vec!["ssh".to_string()];
    if let Some(identity_file) = &ssh_client.identity_file {
        ssh_cmd_parts.extend(["-i".to_string(), identity_file.clone()]);
    }
    if ssh_client.port != 22 {
        ssh_cmd_parts.extend(["-p".to_string(), ssh_client.port.to_string()]);
    }
    // Use same safety options as SSH client
    ssh_cmd_parts.extend([
        "-o".to_string(), "BatchMode=yes".to_string(),
        "-o".to_string(), "ConnectTimeout=10".to_string(),
    ]);

    rsync_args.extend(["-e".to_string(), ssh_cmd_parts.join(" ")]);
    rsync_args.push(local_str.clone());
    rsync_args.push(format!("{}@{}:{}", ssh_client.user, ssh_client.host, remote_str));

    log_status!(
        "deploy",
        "Syncing directory: {} -> {}@{}:{}",
        local_str,
        ssh_client.user,
        ssh_client.host,
        remote_str
    );

    let output = Command::new("rsync").args(&rsync_args).output();
    match output {
        Ok(output) if output.status.success() => Ok(DeployResult::success(0)),
        Ok(output) => Ok(DeployResult::failure(
            output.status.code().unwrap_or(1),
            String::from_utf8_lossy(&output.stderr).to_string(),
        )),
        Err(err) => Ok(DeployResult::failure(1, format!("rsync failed: {}", err))),
    }
}

fn upload_file(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    // Upload to a temporary file in the same directory and atomically replace the destination.
    // This avoids failures like: `scp: ...: Text file busy` when updating an in-use binary.
    scp_file_atomic(ssh_client, local_path, remote_path)
}

/// Core SCP transfer function.
fn scp_transfer(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
    recursive: bool,
) -> Result<DeployResult> {
    let label = if recursive { "directory" } else { "file" };

    // Local deploy: use cp instead of scp
    if ssh_client.is_local {
        log_status!(
            "deploy",
            "Copying {} (local): {} -> {}",
            label,
            local_path.display(),
            remote_path
        );

        let mut cp_args = vec!["-f".to_string()];
        if recursive {
            cp_args.push("-r".to_string());
        }
        // Preserve permissions and timestamps
        cp_args.push("-p".to_string());
        cp_args.push(local_path.to_string_lossy().to_string());
        cp_args.push(remote_path.to_string());

        let output = Command::new("cp").args(&cp_args).output();
        return match output {
            Ok(output) if output.status.success() => Ok(DeployResult::success(0)),
            Ok(output) => Ok(DeployResult::failure(
                output.status.code().unwrap_or(1),
                String::from_utf8_lossy(&output.stderr).to_string(),
            )),
            Err(err) => Ok(DeployResult::failure(1, err.to_string())),
        };
    }

    let deploy_defaults = defaults::load_defaults().deploy;
    let mut scp_args: Vec<String> = deploy_defaults.scp_flags.clone();

    if recursive {
        scp_args.push("-r".to_string());
    }

    if let Some(identity_file) = &ssh_client.identity_file {
        scp_args.extend(["-i".to_string(), identity_file.clone()]);
    }

    if ssh_client.port != deploy_defaults.default_ssh_port {
        scp_args.extend(["-P".to_string(), ssh_client.port.to_string()]);
    }

    scp_args.push(local_path.to_string_lossy().to_string());
    scp_args.push(format!(
        "{}@{}:{}",
        ssh_client.user,
        ssh_client.host,
        shell::quote_path(remote_path)
    ));

    log_status!(
        "deploy",
        "Uploading {}: {} -> {}@{}:{}",
        label,
        local_path.display(),
        ssh_client.user,
        ssh_client.host,
        remote_path
    );

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

fn scp_file(ssh_client: &SshClient, local_path: &Path, remote_path: &str) -> Result<DeployResult> {
    scp_transfer(ssh_client, local_path, remote_path, false)
}

fn scp_file_atomic(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    let remote = Path::new(remote_path);
    let remote_dir = remote.parent().and_then(|p| p.to_str()).unwrap_or(".");
    let remote_filename = remote.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        Error::validation_invalid_argument(
            "remotePath",
            "Remote path must include a file name",
            Some(remote_path.to_string()),
            None,
        )
    })?;

    let tmp_path = format!(
        "{}/.homeboy-upload-{}.tmp.{}",
        remote_dir,
        remote_filename,
        std::process::id()
    );

    let upload_result = scp_transfer(ssh_client, local_path, &tmp_path, false)?;
    if !upload_result.success {
        return Ok(upload_result);
    }

    // Atomic replace: mv temp -> destination (same directory)
    let mv_cmd = format!(
        "mv -f {} {}",
        shell::quote_path(&tmp_path),
        shell::quote_path(remote_path)
    );
    let mv_output = ssh_client.execute(&mv_cmd);

    if !mv_output.success {
        let error_detail = if mv_output.stderr.is_empty() {
            mv_output.stdout
        } else {
            mv_output.stderr
        };
        return Ok(DeployResult::failure(
            mv_output.exit_code,
            format!("Failed to move uploaded file into place: {}", error_detail),
        ));
    }

    Ok(DeployResult::success(0))
}



// =============================================================================
// Deploy Orchestration
// =============================================================================

/// Configuration for deploy orchestration.
#[derive(Debug, Clone)]
pub struct DeployConfig {
    pub component_ids: Vec<String>,
    pub all: bool,
    pub outdated: bool,
    pub dry_run: bool,
    pub check: bool,
    pub force: bool,
    /// Skip build if artifact already exists (used by release --deploy)
    pub skip_build: bool,
    /// Keep build dependencies (skip cleanup even when auto_cleanup is enabled)
    pub keep_deps: bool,
}

/// Reason why a component was selected for deployment.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployReason {
    /// Component was explicitly specified by ID
    ExplicitlySelected,
    /// --all flag was used
    AllSelected,
    /// Local and remote versions differ
    VersionMismatch,
    /// Could not determine local version
    UnknownLocalVersion,
    /// Could not determine remote version (not deployed or no version file)
    UnknownRemoteVersion,
}

/// Status indicator for component version comparison.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    /// Local and remote versions match
    UpToDate,
    /// Local version ahead of remote (needs deploy)
    NeedsUpdate,
    /// Remote version ahead of local (local behind)
    BehindRemote,
    /// Cannot determine status
    Unknown,
}

/// Release state tracking for deployment decisions.
/// Captures git state relative to the last version tag.
#[derive(Debug, Clone, Serialize)]
pub struct ReleaseState {
    /// Number of commits since the last version tag
    pub commits_since_version: u32,
    /// Number of code commits (non-docs)
    #[serde(skip_serializing_if = "utils::is_zero_u32")]
    pub code_commits: u32,
    /// Number of docs-only commits
    #[serde(skip_serializing_if = "utils::is_zero_u32")]
    pub docs_only_commits: u32,
    /// Whether there are uncommitted changes in the working directory
    pub has_uncommitted_changes: bool,
    /// The baseline reference (tag or commit hash) used for comparison
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_ref: Option<String>,
}

/// Result for a single component deployment.
#[derive(Debug, Clone, Serialize)]

pub struct ComponentDeployResult {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_reason: Option<DeployReason>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_status: Option<ComponentStatus>,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub error: Option<String>,
    pub artifact_path: Option<String>,
    pub remote_path: Option<String>,
    pub build_command: Option<String>,
    pub build_exit_code: Option<i32>,
    pub deploy_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_state: Option<ReleaseState>,
}

impl ComponentDeployResult {
    fn new(component: &Component, base_path: &str) -> Self {
        Self {
            id: component.id.clone(),
            status: String::new(),
            deploy_reason: None,
            component_status: None,
            local_version: None,
            remote_version: None,
            error: None,
            artifact_path: component.build_artifact.clone(),
            remote_path: base_path::join_remote_path(Some(base_path), &component.remote_path).ok(),
            build_command: component.build_command.clone(),
            build_exit_code: None,
            deploy_exit_code: None,
            release_state: None,
        }
    }

    /// Shorthand for the common failure pattern: status="failed" + versions + error.
    fn failed(
        component: &Component,
        base_path: &str,
        local_version: Option<String>,
        remote_version: Option<String>,
        error: String,
    ) -> Self {
        Self::new(component, base_path)
            .with_status("failed")
            .with_versions(local_version, remote_version)
            .with_error(error)
    }

    fn with_status(mut self, status: &str) -> Self {
        self.status = status.to_string();
        self
    }

    fn with_versions(mut self, local: Option<String>, remote: Option<String>) -> Self {
        self.local_version = local;
        self.remote_version = remote;
        self
    }

    fn with_error(mut self, error: String) -> Self {
        self.error = Some(error);
        self
    }

    fn with_build_exit_code(mut self, code: Option<i32>) -> Self {
        self.build_exit_code = code;
        self
    }

    fn with_deploy_exit_code(mut self, code: Option<i32>) -> Self {
        self.deploy_exit_code = code;
        self
    }

    fn with_component_status(mut self, status: ComponentStatus) -> Self {
        self.component_status = Some(status);
        self
    }

    fn with_remote_path(mut self, path: String) -> Self {
        self.remote_path = Some(path);
        self
    }

    fn with_release_state(mut self, state: ReleaseState) -> Self {
        self.release_state = Some(state);
        self
    }
}

/// Summary of deploy orchestration.
#[derive(Debug, Clone, Serialize)]

pub struct DeploySummary {
    pub total: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
}

/// Result of deploy orchestration for multiple components.
#[derive(Debug, Clone, Serialize)]

pub struct DeployOrchestrationResult {
    pub results: Vec<ComponentDeployResult>,
    pub summary: DeploySummary,
}

/// High-level deploy entry point. Resolves SSH context internally.
///
/// This is the preferred entry point for callers - it handles project loading
/// and SSH context resolution, keeping those details encapsulated.
pub fn run(project_id: &str, config: &DeployConfig) -> Result<DeployOrchestrationResult> {
    let project = project::load(project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(project_id)?;
    deploy_components(config, &project, &ctx, &base_path)
}

/// Main deploy orchestration entry point.
/// Handles component selection, building, and deployment.
fn deploy_components(
    config: &DeployConfig,
    project: &Project,
    ctx: &RemoteProjectContext,
    base_path: &str,
) -> Result<DeployOrchestrationResult> {
    let all_components = load_project_components(&project.component_ids)?;
    if all_components.is_empty() {
        return Err(Error::validation_invalid_argument(
            "componentIds",
            "No components configured for project",
            None,
            None,
        ));
    }

    let components = plan_components(config, &all_components, base_path, &ctx.client)?;

    if components.is_empty() {
        return Ok(DeployOrchestrationResult {
            results: vec![],
            summary: DeploySummary {
                total: 0,
                succeeded: 0,
                failed: 0,
                skipped: 0,
            },
        });
    }

    // Gather versions
    let local_versions: HashMap<String, String> = components
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();
    let remote_versions = if config.outdated || config.dry_run || config.check {
        fetch_remote_versions(&components, base_path, &ctx.client)
    } else {
        HashMap::new()
    };

    // Check and dry-run modes return early without building or deploying
    if config.check {
        return Ok(run_check_mode(&components, &local_versions, &remote_versions, base_path));
    }
    if config.dry_run {
        return Ok(run_dry_run_mode(&components, &local_versions, &remote_versions, base_path, config));
    }

    if !config.force {
        check_uncommitted_changes(&components)?;
    }

    // Execute deployments
    let mut results: Vec<ComponentDeployResult> = vec![];
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;

    for component in &components {
        let result = execute_component_deploy(
            component,
            config,
            ctx,
            base_path,
            project,
            local_versions.get(&component.id).cloned(),
            remote_versions.get(&component.id).cloned(),
        );
        if result.status == "deployed" {
            succeeded += 1;
        } else {
            failed += 1;
        }
        results.push(result);
    }

    Ok(DeployOrchestrationResult {
        results,
        summary: DeploySummary {
            total: succeeded + failed,
            succeeded,
            failed,
            skipped: 0,
        },
    })
}

/// Check mode: return component status without building or deploying.
fn run_check_mode(
    components: &[Component],
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    base_path: &str,
) -> DeployOrchestrationResult {
    let results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let status = calculate_component_status(c, remote_versions);
            let release_state = calculate_release_state(c);
            let mut result = ComponentDeployResult::new(c, base_path)
                .with_status("checked")
                .with_versions(local_versions.get(&c.id).cloned(), remote_versions.get(&c.id).cloned())
                .with_component_status(status);
            if let Some(state) = release_state {
                result = result.with_release_state(state);
            }
            result
        })
        .collect();

    let total = results.len() as u32;
    DeployOrchestrationResult {
        results,
        summary: DeploySummary { total, succeeded: 0, failed: 0, skipped: 0 },
    }
}

/// Dry-run mode: return planned results without building or deploying.
fn run_dry_run_mode(
    components: &[Component],
    local_versions: &HashMap<String, String>,
    remote_versions: &HashMap<String, String>,
    base_path: &str,
    config: &DeployConfig,
) -> DeployOrchestrationResult {
    let results: Vec<ComponentDeployResult> = components
        .iter()
        .map(|c| {
            let status = if config.check {
                calculate_component_status(c, remote_versions)
            } else {
                ComponentStatus::Unknown
            };
            let mut result = ComponentDeployResult::new(c, base_path)
                .with_status("planned")
                .with_versions(local_versions.get(&c.id).cloned(), remote_versions.get(&c.id).cloned());
            if config.check {
                result = result.with_component_status(status);
            }
            result
        })
        .collect();

    let total = results.len() as u32;
    DeployOrchestrationResult {
        results,
        summary: DeploySummary { total, succeeded: 0, failed: 0, skipped: 0 },
    }
}

/// Verify no components have uncommitted changes before deployment.
fn check_uncommitted_changes(components: &[Component]) -> Result<()> {
    let dirty: Vec<&str> = components
        .iter()
        .filter(|c| !git::is_workdir_clean(Path::new(&c.local_path)))
        .map(|c| c.id.as_str())
        .collect();

    if !dirty.is_empty() {
        return Err(Error::validation_invalid_argument(
            "components",
            format!("Components have uncommitted changes: {}", dirty.join(", ")),
            None,
            Some(vec![
                "Commit your changes before deploying to ensure deployed code is tracked".to_string(),
                "Use --force to deploy anyway".to_string(),
            ]),
        ));
    }
    Ok(())
}

/// Build, deploy, and post-process a single component.
///
/// Returns a `ComponentDeployResult` regardless of success or failure —
/// the caller uses the `status` field to count outcomes.
fn execute_component_deploy(
    component: &Component,
    config: &DeployConfig,
    ctx: &RemoteProjectContext,
    base_path: &str,
    project: &Project,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> ComponentDeployResult {
    let is_git_deploy = component.deploy_strategy.as_deref() == Some("git");

    // Build (git-deploy and skip-build skip this step)
    let (build_exit_code, build_error) = if is_git_deploy || config.skip_build {
        (Some(0), None)
    } else if artifact_is_fresh(component) {
        log_status!("deploy", "Artifact for '{}' is up-to-date, skipping build", component.id);
        (Some(0), None)
    } else {
        build::build_component(component)
    };

    if let Some(ref error) = build_error {
        return ComponentDeployResult::failed(component, base_path, local_version, remote_version, error.clone())
            .with_build_exit_code(build_exit_code);
    }

    // Resolve install directory
    let install_dir = match base_path::join_remote_path(Some(base_path), &component.remote_path) {
        Ok(v) => v,
        Err(err) => {
            return ComponentDeployResult::failed(component, base_path, local_version, remote_version, err.to_string())
                .with_build_exit_code(build_exit_code);
        }
    };

    // Dispatch by deploy strategy
    let strategy = component.deploy_strategy.as_deref().unwrap_or("rsync");

    if strategy == "git" {
        return execute_git_deploy(component, config, ctx, base_path, &install_dir, local_version, remote_version);
    }

    execute_artifact_deploy(
        component, config, ctx, base_path, project, &install_dir,
        local_version, remote_version, build_exit_code,
    )
}

/// Deploy a component via git push strategy.
fn execute_git_deploy(
    component: &Component,
    config: &DeployConfig,
    ctx: &RemoteProjectContext,
    base_path: &str,
    install_dir: &str,
    local_version: Option<String>,
    remote_version: Option<String>,
) -> ComponentDeployResult {
    let git_config = component.git_deploy.clone().unwrap_or_default();
    let deploy_result = deploy_via_git(
        &ctx.client,
        install_dir,
        &git_config,
        local_version.as_deref(),
    );

    match deploy_result {
        Ok(DeployResult { success: true, exit_code, .. }) => {
            if let Ok(Some(summary)) = cleanup_build_dependencies(component, config) {
                log_status!("deploy", "Cleanup: {}", summary);
            }
            run_post_deploy_hooks(&ctx.client, component, install_dir, base_path);

            ComponentDeployResult::new(component, base_path)
                .with_status("deployed")
                .with_versions(local_version.clone(), local_version)
                .with_remote_path(install_dir.to_string())
                .with_deploy_exit_code(Some(exit_code))
        }
        Ok(DeployResult { error, exit_code, .. }) => {
            ComponentDeployResult::failed(component, base_path, local_version, remote_version, error.unwrap_or_default())
                .with_remote_path(install_dir.to_string())
                .with_deploy_exit_code(Some(exit_code))
        }
        Err(err) => {
            ComponentDeployResult::failed(component, base_path, local_version, remote_version, err.to_string())
                .with_remote_path(install_dir.to_string())
        }
    }
}

/// Deploy a component via artifact upload (rsync / extension override).
fn execute_artifact_deploy(
    component: &Component,
    config: &DeployConfig,
    ctx: &RemoteProjectContext,
    base_path: &str,
    project: &Project,
    install_dir: &str,
    local_version: Option<String>,
    remote_version: Option<String>,
    build_exit_code: Option<i32>,
) -> ComponentDeployResult {
    // Resolve artifact path
    let artifact_pattern = match component.build_artifact.as_ref() {
        Some(pattern) => pattern,
        None => {
            return ComponentDeployResult::failed(component, base_path, local_version, remote_version,
                format!("Component '{}' has no build_artifact configured", component.id))
                .with_build_exit_code(build_exit_code);
        }
    };

    let artifact_path = match artifact::resolve_artifact_path(artifact_pattern) {
        Ok(path) => path,
        Err(e) => {
            let error_msg = if config.skip_build {
                format!("{}. Release build may have failed.", e)
            } else {
                format!("{}. Run build first: homeboy build {}", e, component.id)
            };
            return ComponentDeployResult::failed(component, base_path, local_version, remote_version, error_msg)
                .with_build_exit_code(build_exit_code);
        }
    };

    // For self-deploy components (e.g. deploying homeboy itself), prefer the
    // installed binary over a stale build artifact. This handles the case where
    // `homeboy upgrade` installed a new binary but the build artifact is from a
    // previous version — without this, `deploy --shared` would push the old binary.
    let artifact_path = if is_self_deploy(component) {
        match prefer_installed_binary(&artifact_path) {
            Some(installed) => installed,
            None => artifact_path,
        }
    } else {
        artifact_path
    };

    // Look up verification from extensions
    let verification = find_deploy_verification(install_dir);

    // Check for extension-defined deploy override
    let deploy_result =
        if let Some((override_config, extension)) = find_deploy_override(install_dir) {
            deploy_with_override(
                &ctx.client,
                &artifact_path,
                install_dir,
                &override_config,
                &extension,
                verification.as_ref(),
                Some(base_path),
                project.domain.as_deref(),
                component.remote_owner.as_deref(),
            )
        } else {
            deploy_artifact(
                &ctx.client,
                &artifact_path,
                install_dir,
                component.extract_command.as_deref(),
                verification.as_ref(),
                component.remote_owner.as_deref(),
            )
        };

    match deploy_result {
        Ok(DeployResult { success: true, exit_code, .. }) => {
            if let Ok(Some(summary)) = cleanup_build_dependencies(component, config) {
                log_status!("deploy", "Cleanup: {}", summary);
            }
            if is_self_deploy(component) {
                log_status!(
                    "deploy",
                    "Deployed '{}' binary. Remote processes will use the new version on next invocation.",
                    component.id
                );
            }
            run_post_deploy_hooks(&ctx.client, component, install_dir, base_path);

            ComponentDeployResult::new(component, base_path)
                .with_status("deployed")
                .with_versions(local_version.clone(), local_version)
                .with_remote_path(install_dir.to_string())
                .with_build_exit_code(build_exit_code)
                .with_deploy_exit_code(Some(exit_code))
        }
        Ok(DeployResult { success: false, exit_code, error }) => {
            ComponentDeployResult::failed(component, base_path, local_version, remote_version,
                error.unwrap_or_default())
                .with_remote_path(install_dir.to_string())
                .with_build_exit_code(build_exit_code)
                .with_deploy_exit_code(Some(exit_code))
        }
        Err(err) => {
            ComponentDeployResult::failed(component, base_path, local_version, remote_version, err.to_string())
                .with_remote_path(install_dir.to_string())
                .with_build_exit_code(build_exit_code)
        }
    }
}

// =============================================================================
// Cleanup Functions
// =============================================================================

/// Clean up build dependencies from component's local_path after successful deploy.
/// This is a best-effort operation - failures are logged but do not fail the deploy.
fn cleanup_build_dependencies(
    component: &Component,
    config: &DeployConfig,
) -> Result<Option<String>> {
    // Skip cleanup if disabled at component level
    if !component.auto_cleanup {
        return Ok(None);
    }

    // Skip cleanup if --keep-deps flag is set
    if config.keep_deps {
        return Ok(Some("skipped (--keep-deps flag)".to_string()));
    }

    // Collect cleanup paths from linked extensions
    let mut cleanup_paths = Vec::new();
    if let Some(ref extensions) = component.extensions {
        for extension_id in extensions.keys() {
            if let Ok(manifest) = crate::extension::load_extension(extension_id) {
                if let Some(ref build) = manifest.build {
                    cleanup_paths.extend(build.cleanup_paths.iter().cloned());
                }
            }
        }
    }

    if cleanup_paths.is_empty() {
        return Ok(Some(
            "skipped (no cleanup paths configured in extensions)".to_string(),
        ));
    }

    let local_path = Path::new(&component.local_path);
    let mut cleaned_paths = Vec::new();
    let mut total_bytes_freed = 0u64;

    for cleanup_path in &cleanup_paths {
        let full_path = local_path.join(cleanup_path);

        if !full_path.exists() {
            continue;
        }

        // Calculate size before deletion
        let size_before = if full_path.is_dir() {
            calculate_directory_size(&full_path).unwrap_or(0)
        } else {
            full_path.metadata().map(|m| m.len()).unwrap_or(0)
        };

        // Attempt to remove the path
        let cleanup_result = if full_path.is_dir() {
            std::fs::remove_dir_all(&full_path)
        } else {
            std::fs::remove_file(&full_path)
        };

        match cleanup_result {
            Ok(()) => {
                cleaned_paths.push(cleanup_path.clone());
                total_bytes_freed += size_before;
                log_status!(
                    "cleanup",
                    "Removed {} (freed {})",
                    cleanup_path,
                    format_bytes(size_before)
                );
            }
            Err(e) => {
                log_status!(
                    "cleanup",
                    "Warning: failed to remove {}: {}",
                    cleanup_path,
                    e
                );
                // Don't return error - cleanup is best-effort
            }
        }
    }

    if cleaned_paths.is_empty() {
        Ok(Some("no paths needed cleanup".to_string()))
    } else {
        let summary = format!(
            "cleaned {} path(s), freed {}",
            cleaned_paths.len(),
            format_bytes(total_bytes_freed)
        );
        Ok(Some(summary))
    }
}

/// Calculate total size of a directory recursively.
fn calculate_directory_size(path: &Path) -> std::io::Result<u64> {
    let mut total_size = 0;

    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let entry_path = entry.path();

            if entry_path.is_dir() {
                total_size += calculate_directory_size(&entry_path)?;
            } else {
                total_size += entry.metadata()?.len();
            }
        }
    } else {
        total_size = path.metadata()?.len();
    }

    Ok(total_size)
}

/// Format bytes into human-readable format.
fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;

    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", size as u64, UNITS[unit_index])
    } else {
        format!("{:.1} {}", size, UNITS[unit_index])
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

/// Plan which components to deploy based on config flags.
fn plan_components(
    config: &DeployConfig,
    all_components: &[Component],
    base_path: &str,
    client: &SshClient,
) -> Result<Vec<Component>> {
    if !config.component_ids.is_empty() {
        let selected: Vec<Component> = all_components
            .iter()
            .filter(|c| config.component_ids.contains(&c.id))
            .cloned()
            .collect();

        let missing: Vec<String> = config
            .component_ids
            .iter()
            .filter(|id| !selected.iter().any(|c| &c.id == *id))
            .cloned()
            .collect();

        if !missing.is_empty() {
            return Err(Error::validation_invalid_argument(
                "componentIds",
                "Unknown component IDs",
                None,
                Some(missing),
            ));
        }

        if selected.is_empty() {
            return Err(Error::validation_invalid_argument(
                "componentIds",
                "No components selected",
                None,
                None,
            ));
        }

        return Ok(selected);
    }

    if config.check {
        return Ok(all_components.to_vec());
    }

    if config.all {
        return Ok(all_components.to_vec());
    }

    if config.outdated {
        let remote_versions = fetch_remote_versions(all_components, base_path, client);

        let selected: Vec<Component> = all_components
            .iter()
            .filter(|c| {
                let Some(local_version) = version::get_component_version(c) else {
                    return true;
                };

                let Some(remote_version) = remote_versions.get(&c.id) else {
                    return true;
                };

                local_version != *remote_version
            })
            .cloned()
            .collect();

        if selected.is_empty() {
            return Err(Error::validation_invalid_argument(
                "outdated",
                "No outdated components found",
                None,
                None,
            ));
        }

        return Ok(selected);
    }

    Err(Error::validation_missing_argument(vec![
        "component IDs, --all, --outdated, or --check".to_string(),
    ]))
}

/// Calculate component status based on local and remote versions.
fn calculate_component_status(
    component: &Component,
    remote_versions: &HashMap<String, String>,
) -> ComponentStatus {
    let local_version = version::get_component_version(component);
    let remote_version = remote_versions.get(&component.id);

    match (local_version, remote_version) {
        (None, None) => ComponentStatus::Unknown,
        (None, Some(_)) => ComponentStatus::NeedsUpdate,
        (Some(_), None) => ComponentStatus::NeedsUpdate,
        (Some(local), Some(remote)) => {
            if local == *remote {
                ComponentStatus::UpToDate
            } else {
                ComponentStatus::NeedsUpdate
            }
        }
    }
}

/// Calculate release state for a component.
/// Returns commit count since last version tag and uncommitted changes status.
fn calculate_release_state(component: &Component) -> Option<ReleaseState> {
    let path = &component.local_path;

    let baseline = git::detect_baseline_for_path(path).ok()?;

    let commits = git::get_commits_since_tag(path, baseline.reference.as_deref())
        .ok()
        .unwrap_or_default();

    // Categorize commits into code vs docs-only
    let counts = git::categorize_commits(path, &commits);

    let uncommitted = git::get_uncommitted_changes(path)
        .ok()
        .map(|u| u.has_changes)
        .unwrap_or(false);

    Some(ReleaseState {
        commits_since_version: counts.total,
        code_commits: counts.code,
        docs_only_commits: counts.docs_only,
        has_uncommitted_changes: uncommitted,
        baseline_ref: baseline.reference,
    })
}

/// Load components by ID, resolve artifact paths via extension patterns, and filter non-deployable.
///
/// Validates that any extensions declared in the component's `extensions` field are installed.
/// Returns an actionable error with install instructions when extensions are missing,
/// rather than silently skipping the component.
fn load_project_components(component_ids: &[String]) -> Result<Vec<Component>> {
    let mut components = Vec::new();

    for id in component_ids {
        let mut loaded = component::load(id)?;

        // Validate required extensions are installed before attempting artifact resolution.
        // Without this check, missing extensions cause resolve_artifact() to silently
        // return None, and the component gets skipped with a vague "no artifact" message.
        extension::validate_required_extensions(&loaded)?;

        // Resolve effective artifact (component value OR extension pattern)
        let effective_artifact = component::resolve_artifact(&loaded);

        // Git-deploy components don't need a build artifact
        let is_git_deploy = loaded.deploy_strategy.as_deref() == Some("git");

        match effective_artifact {
            Some(artifact) if !is_git_deploy => {
                let resolved_artifact = parser::resolve_path_string(&loaded.local_path, &artifact);
                loaded.build_artifact = Some(resolved_artifact);
                components.push(loaded);
            }
            _ if is_git_deploy => {
                // Git-deploy components are deployable without an artifact
                components.push(loaded);
            }
            Some(_) | None => {
                // Skip - component is intentionally non-deployable
                log_status!(
                    "deploy",
                    "Skipping '{}': no artifact configured (non-deployable component)",
                    loaded.id
                );
                continue;
            }
        }
    }

    Ok(components)
}

/// Check if a component's build artifact is newer than its latest source commit.
///
/// Returns true if the artifact exists and its mtime is after the HEAD commit
/// timestamp, meaning a rebuild would produce the same result.
fn artifact_is_fresh(component: &Component) -> bool {
    let artifact_pattern = match component.build_artifact.as_ref() {
        Some(p) => p,
        None => return false,
    };

    let artifact_path = match artifact::resolve_artifact_path(artifact_pattern) {
        Ok(p) => p,
        Err(_) => return false, // artifact doesn't exist yet
    };

    let artifact_mtime = match artifact_path.metadata().and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return false,
    };

    // Get HEAD commit timestamp as Unix epoch seconds
    let commit_ts = crate::utils::command::run_in_optional(
        &component.local_path,
        "git",
        &["log", "-1", "--format=%ct", "HEAD"],
    );

    let commit_time = match commit_ts {
        Some(ts) => {
            let secs: u64 = match ts.trim().parse() {
                Ok(s) => s,
                Err(_) => return false,
            };
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
        }
        None => return false,
    };

    artifact_mtime > commit_time
}

/// Detect if a component's artifact is a CLI binary matching the currently
/// running process name. Used to print a post-deploy hint for self-deploy.
fn is_self_deploy(component: &Component) -> bool {
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
fn prefer_installed_binary(build_artifact: &Path) -> Option<std::path::PathBuf> {
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
fn fetch_remote_versions(
    components: &[Component],
    base_path: &str,
    client: &SshClient,
) -> HashMap<String, String> {
    let mut versions = HashMap::new();

    for component in components {
        let Some(version_file) = component
            .version_targets
            .as_ref()
            .and_then(|targets| targets.first())
            .map(|t| t.file.as_str())
        else {
            continue;
        };

        let remote_path = match base_path::join_remote_child(
            Some(base_path),
            &component.remote_path,
            version_file,
        ) {
            Ok(value) => value,
            Err(_) => continue,
        };

        let output = client.execute(&format!("cat '{}' 2>/dev/null", remote_path));

        if output.success {
            let pattern = component
                .version_targets
                .as_ref()
                .and_then(|targets| targets.first())
                .and_then(|t| t.pattern.as_deref());

            if let Some(ver) = parse_component_version(&output.stdout, pattern, version_file) {
                versions.insert(component.id.clone(), ver);
            }
        }
    }

    versions
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
fn find_deploy_verification(target_path: &str) -> Option<DeployVerification> {
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
fn find_deploy_override(target_path: &str) -> Option<(DeployOverride, ExtensionManifest)> {
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
fn deploy_with_override(
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
    log_status!("deploy", "Using extension deploy override: {}", extension.id);
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
fn run_post_deploy_hooks(
    ssh_client: &SshClient,
    component: &Component,
    install_dir: &str,
    base_path: &str,
) {
    let mut vars = HashMap::new();
    vars.insert(TemplateVars::COMPONENT_ID.to_string(), component.id.clone());
    vars.insert(TemplateVars::INSTALL_DIR.to_string(), install_dir.to_string());
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
