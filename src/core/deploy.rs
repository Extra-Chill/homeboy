use serde::Serialize;
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

use crate::base_path;
use crate::build;
use crate::component::{self, Component};
use crate::config;
use crate::context::{resolve_project_ssh_with_base_path, RemoteProjectContext};
use crate::defaults;
use crate::error::{Error, Result};
use crate::module::{load_all_modules, DeployVerification};
use crate::permissions;
use crate::project::{self, Project};
use crate::shell;
use crate::ssh::SshClient;
use crate::template::{render_map, TemplateVars};
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
            format!("{}/{}", remote_path, artifact_filename)
        } else {
            remote_path.to_string()
        };

        // Create target directory
        let mkdir_cmd = format!("mkdir -p {}", shell::quote_path(remote_path));
        eprintln!("[deploy] Creating directory: {}", remote_path);
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
            eprintln!("[deploy] Extracting: {}", rendered_cmd);

            let extract_output = ssh_client.execute(&extract_cmd);
            if !extract_output.success {
                return Ok(DeployResult::failure(
                    extract_output.exit_code,
                    format!("Extract command failed: {}", extract_output.stderr),
                ));
            }

            // Fix file permissions after extraction
            eprintln!("[deploy] Fixing file permissions");
            permissions::fix_deployed_permissions(ssh_client, remote_path)?;
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
    eprintln!("[deploy] Creating parent directory: {}", parent);
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
    // Load SCP flags from configurable defaults
    let deploy_defaults = defaults::load_defaults().deploy;
    let mut scp_args: Vec<String> = deploy_defaults.scp_flags.clone();

    if let Some(identity_file) = &ssh_client.identity_file {
        scp_args.push("-i".to_string());
        scp_args.push(identity_file.clone());
    }

    if ssh_client.port != deploy_defaults.default_ssh_port {
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

    eprintln!(
        "[deploy] Uploading: {} -> {}@{}:{}",
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

fn scp_recursive(
    ssh_client: &SshClient,
    local_path: &Path,
    remote_path: &str,
) -> Result<DeployResult> {
    // Load SCP flags from configurable defaults, add -r for recursive
    let deploy_defaults = defaults::load_defaults().deploy;
    let mut scp_args: Vec<String> = deploy_defaults.scp_flags.clone();
    scp_args.push("-r".to_string());

    if let Some(identity_file) = &ssh_client.identity_file {
        scp_args.push("-i".to_string());
        scp_args.push(identity_file.clone());
    }

    if ssh_client.port != deploy_defaults.default_ssh_port {
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

    eprintln!(
        "[deploy] Uploading directory: {} -> {}@{}:{}",
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

/// Result for a single component deployment.
#[derive(Debug, Clone, Serialize)]

pub struct ComponentDeployResult {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deploy_reason: Option<DeployReason>,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub error: Option<String>,
    pub artifact_path: Option<String>,
    pub remote_path: Option<String>,
    pub build_command: Option<String>,
    pub build_exit_code: Option<i32>,
    pub deploy_exit_code: Option<i32>,
}

impl ComponentDeployResult {
    fn new(component: &Component, base_path: &str) -> Self {
        Self {
            id: component.id.clone(),
            status: String::new(),
            deploy_reason: None,
            local_version: None,
            remote_version: None,
            error: None,
            artifact_path: Some(component.build_artifact.clone()),
            remote_path: base_path::join_remote_path(Some(base_path), &component.remote_path).ok(),
            build_command: component.build_command.clone(),
            build_exit_code: None,
            deploy_exit_code: None,
        }
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

    fn with_remote_path(mut self, path: String) -> Self {
        self.remote_path = Some(path);
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
pub fn deploy_components(
    config: &DeployConfig,
    project: &Project,
    ctx: &RemoteProjectContext,
    base_path: &str,
) -> Result<DeployOrchestrationResult> {
    let all_components = load_project_components(&project.component_ids)?;
    if all_components.is_empty() {
        return Err(Error::other(
            "No components configured for project".to_string(),
        ));
    }

    let components_to_deploy = plan_components(config, &all_components, base_path, &ctx.client)?;

    if components_to_deploy.is_empty() {
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

    // Gather local versions
    let local_versions: HashMap<String, String> = components_to_deploy
        .iter()
        .filter_map(|c| version::get_component_version(c).map(|v| (c.id.clone(), v)))
        .collect();

    // Gather remote versions if needed (for --outdated or --dry-run)
    let remote_versions = if config.outdated || config.dry_run {
        fetch_remote_versions(&components_to_deploy, base_path, &ctx.client)
    } else {
        HashMap::new()
    };

    // Dry-run mode: return planned results without building or deploying
    if config.dry_run {
        let results: Vec<ComponentDeployResult> = components_to_deploy
            .iter()
            .map(|c| {
                let local_version = local_versions.get(&c.id).cloned();
                let remote_version = remote_versions.get(&c.id).cloned();
                ComponentDeployResult::new(c, base_path)
                    .with_status("planned")
                    .with_versions(local_version, remote_version)
            })
            .collect();

        let total = results.len() as u32;
        return Ok(DeployOrchestrationResult {
            results,
            summary: DeploySummary {
                total,
                succeeded: 0,
                failed: 0,
                skipped: 0,
            },
        });
    }

    // Execute deployments
    let mut results: Vec<ComponentDeployResult> = vec![];
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;

    for component in &components_to_deploy {
        let local_version = local_versions.get(&component.id).cloned();
        let remote_version = remote_versions.get(&component.id).cloned();

        // Build is mandatory before deploy
        let (build_exit_code, build_error) = build::build_component(component);

        if let Some(ref error) = build_error {
            results.push(
                ComponentDeployResult::new(component, base_path)
                    .with_status("failed")
                    .with_versions(local_version, remote_version)
                    .with_error(error.clone())
                    .with_build_exit_code(build_exit_code),
            );
            failed += 1;
            continue;
        }

        // Check artifact exists after build
        if !Path::new(&component.build_artifact).exists() {
            results.push(
                ComponentDeployResult::new(component, base_path)
                    .with_status("failed")
                    .with_versions(local_version, remote_version)
                    .with_error(format!("Artifact not found: {}", component.build_artifact))
                    .with_build_exit_code(build_exit_code),
            );
            failed += 1;
            continue;
        }

        // Calculate install directory
        let install_dir = match base_path::join_remote_path(Some(base_path), &component.remote_path)
        {
            Ok(v) => v,
            Err(err) => {
                results.push(
                    ComponentDeployResult::new(component, base_path)
                        .with_status("failed")
                        .with_versions(local_version, remote_version)
                        .with_error(err.to_string())
                        .with_build_exit_code(build_exit_code),
                );
                failed += 1;
                continue;
            }
        };

        // Look up verification from modules
        let verification = find_deploy_verification(&install_dir);

        // Deploy using core module
        let deploy_result = deploy_artifact(
            &ctx.client,
            Path::new(&component.build_artifact),
            &install_dir,
            component.extract_command.as_deref(),
            verification.as_ref(),
        );

        match deploy_result {
            Ok(DeployResult {
                success: true,
                exit_code,
                ..
            }) => {
                results.push(
                    ComponentDeployResult::new(component, base_path)
                        .with_status("deployed")
                        .with_versions(local_version.clone(), local_version)
                        .with_remote_path(install_dir)
                        .with_build_exit_code(build_exit_code)
                        .with_deploy_exit_code(Some(exit_code)),
                );
                succeeded += 1;
            }
            Ok(DeployResult {
                success: false,
                exit_code,
                error,
            }) => {
                let mut result = ComponentDeployResult::new(component, base_path)
                    .with_status("failed")
                    .with_versions(local_version, remote_version)
                    .with_remote_path(install_dir)
                    .with_build_exit_code(build_exit_code)
                    .with_deploy_exit_code(Some(exit_code));
                if let Some(e) = error {
                    result = result.with_error(e);
                }
                results.push(result);
                failed += 1;
            }
            Err(err) => {
                results.push(
                    ComponentDeployResult::new(component, base_path)
                        .with_status("failed")
                        .with_versions(local_version, remote_version)
                        .with_remote_path(install_dir)
                        .with_error(err.to_string())
                        .with_build_exit_code(build_exit_code),
                );
                failed += 1;
            }
        }
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
    if config.all {
        return Ok(all_components.to_vec());
    }

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

    Err(Error::other(
        "No components specified. Use component IDs, --all, or --outdated".to_string(),
    ))
}

/// Load components by ID and normalize artifact paths.
fn load_project_components(component_ids: &[String]) -> Result<Vec<Component>> {
    let mut components = Vec::new();

    for id in component_ids {
        let mut loaded = component::load(id)?;
        // Resolve relative build artifact path
        if !loaded.build_artifact.starts_with('/') {
            loaded.build_artifact = format!("{}/{}", loaded.local_path, loaded.build_artifact);
        }
        components.push(loaded);
    }

    Ok(components)
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

/// Parse version from content using pattern or module defaults.
fn parse_component_version(content: &str, pattern: Option<&str>, filename: &str) -> Option<String> {
    let pattern_str = match pattern {
        Some(p) => p.replace("\\\\", "\\"),
        None => version::default_pattern_for_file(filename)?,
    };

    version::parse_version(content, &pattern_str)
}

/// Find deploy verification config from modules.
fn find_deploy_verification(target_path: &str) -> Option<DeployVerification> {
    for module in load_all_modules() {
        for verification in &module.deploy {
            if target_path.contains(&verification.path_pattern) {
                return Some(verification.clone());
            }
        }
    }
    None
}
