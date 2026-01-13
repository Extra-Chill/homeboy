use clap::Args;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

use homeboy::component;
use homeboy::project::{self, ProjectRecord};
use homeboy::context::{resolve_project_ssh_with_base_path, RemoteProjectContext};
use homeboy::deploy::{deploy_artifact, parse_bulk_component_ids, DeployResult};
use homeboy::module::{load_module, DeployVerification};
use homeboy::ssh::{execute_local_command_in_dir, SshClient};
use homeboy::version::{default_pattern_for_file, parse_version};

use super::CmdResult;

type ProjectLoader = fn(&str) -> homeboy::Result<ProjectRecord>;
type SshResolver = fn(&str) -> homeboy::Result<(RemoteProjectContext, String)>;
type BuildRunner = fn(&Component) -> (Option<i32>, Option<String>);

#[derive(Args)]
pub struct DeployArgs {
    /// Project ID
    pub project_id: String,

    /// JSON input spec for bulk operations
    #[arg(long)]
    pub json: Option<String>,

    /// Component IDs to deploy
    pub component_ids: Vec<String>,

    /// Deploy all configured components
    #[arg(long)]
    pub all: bool,

    /// Deploy only outdated components
    #[arg(long)]
    pub outdated: bool,

    /// Show what would be deployed without executing
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployComponentResult {
    pub id: String,
    pub name: String,
    pub status: String,
    pub local_version: Option<String>,
    pub remote_version: Option<String>,
    pub error: Option<String>,
    pub artifact_path: Option<String>,
    pub remote_path: Option<String>,
    pub build_command: Option<String>,
    pub build_exit_code: Option<i32>,
    pub deploy_exit_code: Option<i32>,
}

impl DeployComponentResult {
    fn new(component: &Component, base_path: &str) -> Self {
        Self {
            id: component.id.clone(),
            name: component.name.clone(),
            status: String::new(),
            local_version: None,
            remote_version: None,
            error: None,
            artifact_path: Some(component.build_artifact.clone()),
            remote_path: homeboy::base_path::join_remote_path(
                Some(base_path),
                &component.remote_path,
            )
            .ok(),
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

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeploySummary {
    pub succeeded: u32,
    pub failed: u32,
    pub skipped: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeployOutput {
    pub command: String,
    pub project_id: String,
    pub all: bool,
    pub outdated: bool,
    pub dry_run: bool,
    pub components: Vec<DeployComponentResult>,
    pub summary: DeploySummary,
}

pub fn run(args: DeployArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DeployOutput> {
    run_with_loaders(
        args,
        project::load_record,
        resolve_project_ssh_with_base_path,
        run_build,
    )
}

fn run_with_loaders(
    mut args: DeployArgs,
    project_loader: ProjectLoader,
    ssh_resolver: SshResolver,
    build_runner: BuildRunner,
) -> CmdResult<DeployOutput> {
    // Check for common subcommand mistakes (deploy doesn't have subcommands)
    let subcommand_hints = ["status", "list", "show", "help"];
    if subcommand_hints.contains(&args.project_id.as_str()) {
        return Err(homeboy::Error::validation_invalid_argument(
            "project_id",
            format!(
                "'{}' looks like a subcommand, but 'deploy' doesn't have subcommands. \
                 Usage: homeboy deploy <projectId> [componentIds...] [--all] [--dry-run]",
                args.project_id
            ),
            None,
            None,
        ));
    }

    // Parse JSON input if provided and merge into component_ids
    if let Some(ref spec) = args.json {
        args.component_ids = parse_bulk_component_ids(spec)?;
    }

    let project = project_loader(&args.project_id)?;
    let (ctx, base_path) = ssh_resolver(&args.project_id)?;
    let client = ctx.client;

    let all_components = load_components(&project.config.component_ids);
    if all_components.is_empty() {
        return Err(homeboy::Error::other(
            "No components configured for project".to_string(),
        ));
    }

    let components_to_deploy =
        plan_components_to_deploy(&args, &all_components, &base_path, &client)?;

    if components_to_deploy.is_empty() {
        return Ok((
            DeployOutput {
                command: "deploy.run".to_string(),
                project_id: args.project_id,
                all: args.all,
                outdated: args.outdated,
                dry_run: args.dry_run,
                components: vec![],
                summary: DeploySummary {
                    succeeded: 0,
                    failed: 0,
                    skipped: 0,
                },
            },
            0,
        ));
    }

    let local_versions: HashMap<String, String> = components_to_deploy
        .iter()
        .filter_map(|c| fetch_local_version(c).map(|v| (c.id.clone(), v)))
        .collect();

    let remote_versions = if args.dry_run || args.outdated {
        fetch_remote_versions(&components_to_deploy, &base_path, &client)
    } else {
        HashMap::new()
    };

    if args.dry_run {
        let results = components_to_deploy
            .iter()
            .map(|component| {
                DeployComponentResult::new(component, &base_path)
                    .with_status("would_deploy")
                    .with_versions(
                        local_versions.get(&component.id).cloned(),
                        remote_versions.get(&component.id).cloned(),
                    )
            })
            .collect::<Vec<_>>();

        let succeeded = results.len() as u32;

        return Ok((
            DeployOutput {
                command: "deploy.run".to_string(),
                project_id: args.project_id,
                all: args.all,
                outdated: args.outdated,
                dry_run: true,
                components: results,
                summary: DeploySummary {
                    succeeded,
                    failed: 0,
                    skipped: 0,
                },
            },
            0,
        ));
    }

    let mut results: Vec<DeployComponentResult> = vec![];
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;

    for component in &components_to_deploy {
        let local_version = local_versions.get(&component.id).cloned();
        let remote_version = remote_versions.get(&component.id).cloned();

        // Build is mandatory before deploy
        let (build_exit_code, build_error) = build_runner(component);

        if let Some(ref error) = build_error {
            results.push(
                DeployComponentResult::new(component, &base_path)
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
                DeployComponentResult::new(component, &base_path)
                    .with_status("failed")
                    .with_versions(local_version, remote_version)
                    .with_error(format!("Artifact not found: {}", component.build_artifact))
                    .with_build_exit_code(build_exit_code),
            );
            failed += 1;
            continue;
        }

        // Calculate install directory
        let install_dir = match homeboy::base_path::join_remote_path(
            Some(&base_path),
            &component.remote_path,
        ) {
            Ok(v) => v,
            Err(err) => {
                results.push(
                    DeployComponentResult::new(component, &base_path)
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
        let verification = find_deploy_verification(&project.config.modules, &install_dir);

        // Deploy using core module
        let deploy_result = deploy_artifact(
            &client,
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
                    DeployComponentResult::new(component, &base_path)
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
                let mut result = DeployComponentResult::new(component, &base_path)
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
                    DeployComponentResult::new(component, &base_path)
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

    let exit_code = if failed > 0 { 1 } else { 0 };

    Ok((
        DeployOutput {
            command: "deploy.run".to_string(),
            project_id: args.project_id,
            all: args.all,
            outdated: args.outdated,
            dry_run: args.dry_run,
            components: results,
            summary: DeploySummary {
                succeeded,
                failed,
                skipped: 0,
            },
        },
        exit_code,
    ))
}

#[derive(Clone)]
struct VersionTarget {
    file: String,
    pattern: Option<String>,
}

#[derive(Clone)]
struct Component {
    id: String,
    name: String,
    local_path: String,
    remote_path: String,
    build_artifact: String,
    build_command: Option<String>,
    extract_command: Option<String>,
    version_targets: Option<Vec<VersionTarget>>,
    modules: Vec<String>,
}

fn plan_components_to_deploy(
    args: &DeployArgs,
    all_components: &[Component],
    base_path: &str,
    client: &SshClient,
) -> homeboy::Result<Vec<Component>> {
    if args.all {
        return Ok(all_components.to_vec());
    }

    if !args.component_ids.is_empty() {
        let selected: Vec<Component> = all_components
            .iter()
            .filter(|c| args.component_ids.contains(&c.id))
            .cloned()
            .collect();
        return Ok(selected);
    }

    if args.outdated {
        let remote_versions = fetch_remote_versions(all_components, base_path, client);

        let selected: Vec<Component> = all_components
            .iter()
            .filter(|c| {
                let Some(local_version) = fetch_local_version(c) else {
                    return true;
                };

                let Some(remote_version) = remote_versions.get(&c.id) else {
                    return true;
                };

                local_version != *remote_version
            })
            .cloned()
            .collect();

        return Ok(selected);
    }

    Err(homeboy::Error::other(
        "No components specified. Use component IDs, --all, or --outdated".to_string(),
    ))
}

/// Build is mandatory before deploy. Returns error if no build command configured.
fn run_build(component: &Component) -> (Option<i32>, Option<String>) {
    let build_cmd = component.build_command.clone().or_else(|| {
        homeboy::build::detect_build_command(
            &component.local_path,
            &component.build_artifact,
            &component.modules,
        )
        .map(|candidate| candidate.command)
    });

    let Some(build_cmd) = build_cmd else {
        return (
            Some(1),
            Some(format!(
                "Component '{}' has no build command configured. Configure one with: homeboy component set {} --build-command '<command>'",
                component.id,
                component.id
            )),
        );
    };

    let output = execute_local_command_in_dir(&build_cmd, Some(&component.local_path));

    if output.success {
        (Some(output.exit_code), None)
    } else {
        (
            Some(output.exit_code),
            Some(format!(
                "Build failed for '{}'. Fix build errors before deploying.",
                component.id
            )),
        )
    }
}

fn find_deploy_verification(modules: &[String], target_path: &str) -> Option<DeployVerification> {
    for module_id in modules {
        if let Some(module) = load_module(module_id) {
            for verification in &module.deploy {
                if target_path.contains(&verification.path_pattern) {
                    return Some(verification.clone());
                }
            }
        }
    }
    None
}

fn load_components(component_ids: &[String]) -> Vec<Component> {
    let mut components = Vec::new();

    for id in component_ids {
        if let Ok(loaded) = component::load(id) {
            let local_path = loaded.local_path;

            let build_artifact = if loaded.build_artifact.starts_with('/') {
                loaded.build_artifact
            } else {
                format!("{}/{}", local_path, loaded.build_artifact)
            };

            let version_targets = loaded.version_targets.map(|targets| {
                targets
                    .into_iter()
                    .map(|target| VersionTarget {
                        file: target.file,
                        pattern: target.pattern,
                    })
                    .collect::<Vec<_>>()
            });

            components.push(Component {
                id: id.clone(),
                name: loaded.name,
                local_path,
                remote_path: loaded.remote_path,
                build_artifact,
                build_command: loaded.build_command,
                extract_command: loaded.extract_command,
                version_targets,
                modules: loaded.modules,
            });
        }
    }

    components
}

fn parse_component_version(
    content: &str,
    pattern: Option<&str>,
    filename: &str,
    modules: &[String],
) -> Option<String> {
    let pattern_str = match pattern {
        Some(p) => p.replace("\\\\", "\\"),
        None => default_pattern_for_file(filename, modules)?,
    };

    parse_version(content, &pattern_str)
}

fn fetch_local_version(component: &Component) -> Option<String> {
    let target = component.version_targets.as_ref()?.first()?;
    let path = format!("{}/{}", component.local_path, target.file);
    let content = fs::read_to_string(&path).ok()?;
    parse_component_version(
        &content,
        target.pattern.as_deref(),
        &target.file,
        &component.modules,
    )
}

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

        let remote_path = match homeboy::base_path::join_remote_child(
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

            if let Some(version) =
                parse_component_version(&output.stdout, pattern, version_file, &component.modules)
            {
                versions.insert(component.id.clone(), version);
            }
        }
    }

    versions
}
