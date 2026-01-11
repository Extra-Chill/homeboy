use clap::Args;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(test)]
static TEST_SCP_CALL_COUNT: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
fn reset_test_scp_call_count() {
    TEST_SCP_CALL_COUNT.store(0, Ordering::Relaxed);
}

use homeboy_core::config::{ConfigManager, ServerConfig};
use homeboy_core::context::resolve_project_ssh_with_base_path;
use homeboy_core::ssh::{CommandOutput, SshClient};
use homeboy_core::version::parse_version;

fn sanitize_remote_single_quotes(value: &str) -> String {
    value.replace("'", "'\\''")
}

use super::CmdResult;

#[derive(Args)]
pub struct DeployArgs {
    /// Project ID
    pub project_id: String,

    /// Component IDs to deploy
    pub component_ids: Vec<String>,

    /// Deploy all configured components
    #[arg(long)]
    pub all: bool,

    /// Deploy only outdated components
    #[arg(long)]
    pub outdated: bool,

    /// Build components before deploying
    #[arg(long)]
    pub build: bool,

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
    pub scp_exit_code: Option<i32>,
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
    pub project_id: String,
    pub all: bool,
    pub outdated: bool,
    pub build: bool,
    pub dry_run: bool,
    pub components: Vec<DeployComponentResult>,
    pub summary: DeploySummary,
}

pub fn run(args: DeployArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<DeployOutput> {
    let project = ConfigManager::load_project_record(&args.project_id)?;
    let (ctx, base_path) = resolve_project_ssh_with_base_path(&args.project_id)?;
    let server = ctx.server;
    let client = ctx.client;

    let all_components = load_components(&project.config.component_ids);
    if all_components.is_empty() {
        return Err(homeboy_core::Error::other(
            "No components configured for project".to_string(),
        ));
    }

    let components_to_deploy =
        plan_components_to_deploy(&args, &all_components, &server, &base_path, &client)?;

    if components_to_deploy.is_empty() {
        return Ok((
            DeployOutput {
                project_id: args.project_id,
                all: args.all,
                outdated: args.outdated,
                build: args.build,
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
        fetch_remote_versions(
            &components_to_deploy,
            &server,
            &base_path,
            &client as &dyn RemoteExec,
        )
    } else {
        HashMap::new()
    };

    let skipped: u32 = 0;

    if args.dry_run {
        let results = components_to_deploy
            .iter()
            .map(|component| DeployComponentResult {
                id: component.id.clone(),
                name: component.name.clone(),
                status: "would_deploy".to_string(),
                local_version: local_versions.get(&component.id).cloned(),
                remote_version: remote_versions.get(&component.id).cloned(),
                error: None,
                artifact_path: Some(component.build_artifact.clone()),
                remote_path: Some(homeboy_core::base_path::join_remote_path_or_fallback(
                    Some(&base_path),
                    &component.remote_path,
                )),
                build_command: component.build_command.clone(),
                build_exit_code: None,
                scp_exit_code: None,
            })
            .collect::<Vec<_>>();

        let succeeded = results.len() as u32;

        return Ok((
            DeployOutput {
                project_id: args.project_id,
                all: args.all,
                outdated: args.outdated,
                build: args.build,
                dry_run: true,
                components: results,
                summary: DeploySummary {
                    succeeded,
                    failed: 0,
                    skipped,
                },
            },
            0,
        ));
    }

    let mut results: Vec<DeployComponentResult> = vec![];
    let mut succeeded: u32 = 0;
    let mut failed: u32 = 0;
    let skipped: u32 = 0;

    for component in &components_to_deploy {
        let local_version = local_versions.get(&component.id).cloned();
        let remote_version = remote_versions.get(&component.id).cloned();

        let (build_exit_code, build_error) = if args.build {
            run_build_if_configured(component)
        } else {
            (None, None)
        };

        if let Some(ref error) = build_error {
            results.push(DeployComponentResult {
                id: component.id.clone(),
                name: component.name.clone(),
                status: "failed".to_string(),
                local_version,
                remote_version,
                error: Some(error.clone()),
                artifact_path: Some(component.build_artifact.clone()),
                remote_path: Some(homeboy_core::base_path::join_remote_path_or_fallback(
                    Some(&base_path),
                    &component.remote_path,
                )),
                build_command: component.build_command.clone(),
                build_exit_code,
                scp_exit_code: None,
            });
            failed += 1;
            continue;
        }

        if !Path::new(&component.build_artifact).exists() {
            results.push(DeployComponentResult {
                id: component.id.clone(),
                name: component.name.clone(),
                status: "failed".to_string(),
                local_version,
                remote_version,
                error: Some(format!("Artifact not found: {}", component.build_artifact)),
                artifact_path: Some(component.build_artifact.clone()),
                remote_path: Some(homeboy_core::base_path::join_remote_path_or_fallback(
                    Some(&base_path),
                    &component.remote_path,
                )),
                build_command: component.build_command.clone(),
                build_exit_code,
                scp_exit_code: None,
            });
            failed += 1;
            continue;
        }

        let (scp_exit_code, scp_error) =
            deploy_component_artifact(&server, &client, &base_path, component);

        if let Some(error) = scp_error {
            results.push(DeployComponentResult {
                id: component.id.clone(),
                name: component.name.clone(),
                status: "failed".to_string(),
                local_version,
                remote_version,
                error: Some(error),
                artifact_path: Some(component.build_artifact.clone()),
                remote_path: Some(homeboy_core::base_path::join_remote_path_or_fallback(
                    Some(&base_path),
                    &component.remote_path,
                )),
                build_command: component.build_command.clone(),
                build_exit_code,
                scp_exit_code,
            });
            failed += 1;
            continue;
        }

        results.push(DeployComponentResult {
            id: component.id.clone(),
            name: component.name.clone(),
            status: "deployed".to_string(),
            local_version: local_version.clone(),
            remote_version: local_version,
            error: None,
            artifact_path: Some(component.build_artifact.clone()),
            remote_path: Some(homeboy_core::base_path::join_remote_path_or_fallback(
                Some(&base_path),
                &component.remote_path,
            )),
            build_command: component.build_command.clone(),
            build_exit_code,
            scp_exit_code,
        });
        succeeded += 1;
    }

    let exit_code = if failed > 0 { 1 } else { 0 };

    Ok((
        DeployOutput {
            project_id: args.project_id,
            all: args.all,
            outdated: args.outdated,
            build: args.build,
            dry_run: args.dry_run,
            components: results,
            summary: DeploySummary {
                succeeded,
                failed,
                skipped,
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
    version_targets: Option<Vec<VersionTarget>>,
}

fn plan_components_to_deploy(
    args: &DeployArgs,
    all_components: &[Component],
    server: &ServerConfig,
    base_path: &str,
    client: &dyn RemoteExec,
) -> homeboy_core::Result<Vec<Component>> {
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
        let remote_versions = fetch_remote_versions(all_components, server, base_path, client);

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

    Err(homeboy_core::Error::other(
        "No components specified. Use component IDs, --all, or --outdated".to_string(),
    ))
}

fn run_build_if_configured(component: &Component) -> (Option<i32>, Option<String>) {
    let build_cmd = component.build_command.clone().or_else(|| {
        homeboy_core::build::detect_build_command(&component.local_path, &component.build_artifact)
            .map(|candidate| candidate.command)
    });

    let Some(build_cmd) = build_cmd else {
        return (None, None);
    };

    #[cfg(windows)]
    let status = Command::new("cmd")
        .args(["/C", &build_cmd])
        .current_dir(&component.local_path)
        .status();

    #[cfg(not(windows))]
    let status = Command::new("sh")
        .args(["-c", &build_cmd])
        .current_dir(&component.local_path)
        .status();

    match status {
        Ok(status) => {
            if status.success() {
                (Some(status.code().unwrap_or(0)), None)
            } else {
                (
                    Some(status.code().unwrap_or(1)),
                    Some(format!("Build failed for {}", component.id)),
                )
            }
        }
        Err(err) => (Some(1), Some(format!("Build error: {}", err))),
    }
}

fn deploy_component_artifact(
    server: &ServerConfig,
    client: &dyn RemoteExec,
    base_path: &str,
    component: &Component,
) -> (Option<i32>, Option<String>) {
    let install_dir =
        match homeboy_core::base_path::join_remote_path(Some(base_path), &component.remote_path) {
            Ok(value) => value,
            Err(err) => return (Some(1), Some(err.to_string())),
        };

    if component.build_artifact.ends_with(".zip") {
        let zip_filename = Path::new(&component.build_artifact)
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!(".homeboy-{}", name))
            .unwrap_or_else(|| format!(".homeboy-{}.zip", component.id));

        let zip_root_dir =
            homeboy_core::build::detect_zip_single_root_dir(Path::new(&component.build_artifact))
                .ok()
                .flatten();

        let install_basename = Path::new(&install_dir)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default();

        let (unzip_target_dir, final_install_dir) = if zip_root_dir
            .as_deref()
            .is_some_and(|root| root == install_basename)
        {
            let parent = Path::new(&install_dir)
                .parent()
                .and_then(|value| value.to_str())
                .unwrap_or(&install_dir)
                .to_string();
            (parent, install_dir.clone())
        } else {
            (install_dir.clone(), install_dir.clone())
        };

        let upload_dir = if unzip_target_dir != final_install_dir {
            unzip_target_dir.clone()
        } else {
            install_dir.clone()
        };

        let upload_path = match homeboy_core::base_path::join_remote_child(
            Some(base_path),
            &upload_dir,
            &zip_filename,
        ) {
            Ok(value) => value,
            Err(err) => return (Some(1), Some(err.to_string())),
        };

        let mkdir_cmd = match homeboy_core::shell::cd_and(
            "/",
            &format!(
                "mkdir -p '{}'",
                sanitize_remote_single_quotes(&unzip_target_dir)
            ),
        ) {
            Ok(value) => value,
            Err(err) => return (Some(1), Some(err.to_string())),
        };

        let mkdir_output = client.execute(&mkdir_cmd);
        if !mkdir_output.success {
            return (Some(mkdir_output.exit_code), Some(mkdir_output.stderr));
        }

        let (scp_exit_code, scp_error) =
            upload_to_path(server, client, &component.build_artifact, &upload_path);
        if scp_error.is_some() {
            return (scp_exit_code, scp_error);
        }

        let cleanup_target_dir = if let Some(ref root) = zip_root_dir {
            if unzip_target_dir != final_install_dir {
                match homeboy_core::base_path::join_remote_child(None, &unzip_target_dir, root) {
                    Ok(value) => value,
                    Err(_) => final_install_dir.clone(),
                }
            } else {
                final_install_dir.clone()
            }
        } else {
            final_install_dir.clone()
        };

        if cleanup_target_dir.starts_with(base_path)
            && cleanup_target_dir.contains("/wp-content/plugins/")
            && cleanup_target_dir != base_path
        {
            let cleanup_cmd = match homeboy_core::shell::cd_and(
                "/",
                &format!(
                    "rm -rf '{}' && mkdir -p '{}'",
                    sanitize_remote_single_quotes(&cleanup_target_dir),
                    sanitize_remote_single_quotes(&cleanup_target_dir)
                ),
            ) {
                Ok(value) => value,
                Err(err) => return (Some(1), Some(err.to_string())),
            };

            let cleanup_output = client.execute(&cleanup_cmd);
            if !cleanup_output.success {
                return (Some(cleanup_output.exit_code), Some(cleanup_output.stderr));
            }

            let unzip_cmd = match homeboy_core::shell::cd_and(
                &unzip_target_dir,
                &format!(
                    "unzip -o '{}' && rm '{}'",
                    sanitize_remote_single_quotes(&upload_path),
                    sanitize_remote_single_quotes(&upload_path)
                ),
            ) {
                Ok(value) => value,
                Err(err) => return (Some(1), Some(err.to_string())),
            };

            let unzip_output = client.execute(&unzip_cmd);
            if !unzip_output.success {
                return (Some(unzip_output.exit_code), Some(unzip_output.stderr));
            }

            let plugin_check_cmd = match homeboy_core::shell::cd_and(
                "/",
                &format!(
                    "find '{}' -maxdepth 2 -type f -name '*.php' -exec grep -l 'Plugin Name:' {} + | head -n 1",
                    sanitize_remote_single_quotes(&cleanup_target_dir),
                    "\\{}"
                ),
            ) {
                Ok(value) => value,
                Err(err) => return (Some(1), Some(err.to_string())),
            };

            let plugin_check_output = client.execute(&plugin_check_cmd);
            if !plugin_check_output.success {
                return (
                    Some(1),
                    Some(format!(
                        "Deploy completed but plugin verification command failed: {}",
                        plugin_check_output.stderr
                    )),
                );
            }

            if plugin_check_output.stdout.trim().is_empty() {
                return (
                    Some(1),
                    Some(format!(
                        "Deploy completed but no WordPress plugin header found in {}",
                        cleanup_target_dir
                    )),
                );
            }

            return (Some(0), None);
        } else {
            return (
                Some(1),
                Some(format!(
                    "Unsafe deploy cleanup target: {}",
                    cleanup_target_dir
                )),
            );
        }
    }

    upload_to_path(server, client, &component.build_artifact, &install_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make_test_zip_with_root_dir(root_dir: &str) -> tempfile::NamedTempFile {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        let file = std::fs::File::create(temp_file.path()).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::FileOptions::default();

        zip.add_directory(format!("{}/", root_dir), options)
            .unwrap();
        zip.start_file(format!("{}/{}.php", root_dir, root_dir), options)
            .unwrap();
        zip.write_all(b"<?php\n/*\nPlugin Name: Test Plugin\n*/\n")
            .unwrap();
        zip.finish().unwrap();

        temp_file
    }

    #[test]
    fn zip_deploy_unzips_into_parent_when_zip_root_matches_install_dir() {
        reset_test_scp_call_count();

        let zip_file = make_test_zip_with_root_dir("sell-my-images");

        assert_eq!(
            homeboy_core::build::detect_zip_single_root_dir(zip_file.path())
                .unwrap()
                .as_deref(),
            Some("sell-my-images")
        );

        let component = Component {
            id: "sell-my-images".to_string(),
            name: "Sell My Images".to_string(),
            local_path: "/tmp".to_string(),
            remote_path: "wp-content/plugins/sell-my-images".to_string(),
            build_artifact: zip_file.path().to_string_lossy().to_string(),
            build_command: None,
            version_targets: None,
        };

        let client = TestRemoteExec::default();

        let (exit_code, error) =
            deploy_component_artifact_for_test(&client, "/var/www/site", &component);
        assert_eq!(exit_code, Some(0));
        assert!(error.is_none());

        assert_eq!(
            TEST_SCP_CALL_COUNT.load(Ordering::Relaxed),
            1,
            "expected exactly one upload attempt"
        );
    }
}

fn upload_to_path(
    _server: &ServerConfig,
    client: &dyn RemoteExec,
    local_path: &str,
    remote_path: &str,
) -> (Option<i32>, Option<String>) {
    #[cfg(test)]
    {
        TEST_SCP_CALL_COUNT.fetch_add(1, Ordering::Relaxed);
    }

    let Some(ssh_client) = client.as_ssh_client() else {
        #[cfg(test)]
        return (Some(0), None);

        #[cfg(not(test))]
        return (
            Some(1),
            Some("Upload requires SSH client configuration".to_string()),
        );
    };

    let (scp_exit_code, scp_error) = scp_to_path(ssh_client, local_path, remote_path);
    if scp_error.is_none() {
        return (scp_exit_code, None);
    }

    let fallback_output = ssh_client.upload_file(local_path, remote_path);
    if fallback_output.success {
        (Some(fallback_output.exit_code), None)
    } else {
        (
            scp_exit_code,
            Some(format!(
                "SCP failed, and SSH upload fallback failed. scp_error: {} fallback_error: {}",
                scp_error.unwrap_or_default(),
                fallback_output.stderr
            )),
        )
    }
}

fn scp_to_path(
    ssh_client: &SshClient,
    local_path: &str,
    remote_path: &str,
) -> (Option<i32>, Option<String>) {
    let mut scp_args: Vec<String> = vec![];

    if let Some(identity_file) = &ssh_client.identity_file {
        scp_args.push("-i".to_string());
        scp_args.push(identity_file.clone());
    }

    if ssh_client.port != 22 {
        scp_args.push("-P".to_string());
        scp_args.push(ssh_client.port.to_string());
    }

    scp_args.push(local_path.to_string());
    scp_args.push(format!(
        "{}@{}:'{}'",
        ssh_client.user,
        ssh_client.host,
        remote_path.replace("'", "'\\''")
    ));

    let output = Command::new("scp").args(&scp_args).output();

    match output {
        Ok(output) if output.status.success() => (Some(output.status.code().unwrap_or(0)), None),
        Ok(output) => (
            Some(output.status.code().unwrap_or(1)),
            Some(String::from_utf8_lossy(&output.stderr).to_string()),
        ),
        Err(err) => (Some(1), Some(err.to_string())),
    }
}

fn load_components(component_ids: &[String]) -> Vec<Component> {
    let mut components = Vec::new();

    for id in component_ids {
        if let Ok(component) = ConfigManager::load_component(id) {
            let local_path = component.local_path;

            let build_artifact = if component.build_artifact.starts_with('/') {
                component.build_artifact
            } else {
                format!("{}/{}", local_path, component.build_artifact)
            };

            let version_targets = component.version_targets.map(|targets| {
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
                name: component.name,
                local_path,
                remote_path: component.remote_path,
                build_artifact,
                build_command: component.build_command,
                version_targets,
            });
        }
    }

    components
}

fn parse_component_version(content: &str, pattern: Option<&str>) -> Option<String> {
    let default_pattern = r"Version:\s*(\d+\.\d+\.\d+)";

    let pattern_str = match pattern {
        Some(p) => p.replace("\\\\", "\\"),
        None => default_pattern.to_string(),
    };

    parse_version(content, &pattern_str)
}

fn fetch_local_version(component: &Component) -> Option<String> {
    let target = component.version_targets.as_ref()?.first()?;
    let path = format!("{}/{}", component.local_path, target.file);
    let content = fs::read_to_string(&path).ok()?;
    parse_component_version(&content, target.pattern.as_deref())
}

trait RemoteExec {
    fn execute(&self, command: &str) -> CommandOutput;
    fn as_ssh_client(&self) -> Option<&SshClient>;
}

#[cfg(test)]
fn deploy_component_artifact_for_test(
    client: &dyn RemoteExec,
    base_path: &str,
    component: &Component,
) -> (Option<i32>, Option<String>) {
    let server = ServerConfig {
        id: "test".to_string(),
        name: "Test".to_string(),
        host: "example.com".to_string(),
        user: "user".to_string(),
        port: 22,
        identity_file: None,
    };

    deploy_component_artifact(&server, client, base_path, component)
}

#[cfg(test)]
#[derive(Default)]
struct TestRemoteExec {
    commands: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl RemoteExec for TestRemoteExec {
    fn execute(&self, command: &str) -> CommandOutput {
        {
            let mut locked = self.commands.lock().unwrap();
            locked.push(command.to_string());
        }

        CommandOutput {
            stdout: "plugin.php".to_string(),
            stderr: String::new(),
            success: true,
            exit_code: 0,
        }
    }

    fn as_ssh_client(&self) -> Option<&SshClient> {
        None
    }
}

impl RemoteExec for SshClient {
    fn execute(&self, command: &str) -> CommandOutput {
        SshClient::execute(self, command)
    }

    fn as_ssh_client(&self) -> Option<&SshClient> {
        Some(self)
    }
}

fn fetch_remote_versions(
    components: &[Component],
    _server: &ServerConfig,
    base_path: &str,
    client: &dyn RemoteExec,
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

        let remote_path = match homeboy_core::base_path::join_remote_child(
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

            if let Some(version) = parse_component_version(&output.stdout, pattern) {
                versions.insert(component.id.clone(), version);
            }
        }
    }

    versions
}
