use clap::Args;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use homeboy_core::config::{ConfigManager, AppPaths, ServerConfig};
use homeboy_core::output::{print_success, print_error};
use homeboy_core::version::parse_version;

#[derive(Args)]
pub struct DeployArgs {
    /// Project ID
    pub project_id: String,

    /// Component IDs to deploy
    #[arg(trailing_var_arg = true)]
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

    /// Output as JSON
    #[arg(long)]
    pub json: bool,
}

#[derive(Serialize)]
struct ComponentResult {
    id: String,
    name: String,
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration: Option<f64>,
    #[serde(rename = "localVersion", skip_serializing_if = "Option::is_none")]
    local_version: Option<String>,
    #[serde(rename = "remoteVersion", skip_serializing_if = "Option::is_none")]
    remote_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Serialize)]
struct DeployResult {
    components: Vec<ComponentResult>,
    summary: DeploySummary,
}

#[derive(Serialize)]
struct DeploySummary {
    succeeded: u32,
    failed: u32,
    skipped: u32,
}

pub fn run(args: DeployArgs) {
    let project = match ConfigManager::load_project(&args.project_id) {
        Ok(p) => p,
        Err(e) => {
            if args.json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let server_id = match &project.server_id {
        Some(id) => id.clone(),
        None => {
            let msg = format!("Server not configured for project '{}'", args.project_id);
            if args.json { print_error("SERVER_NOT_CONFIGURED", &msg); }
            else { eprintln!("Error: {}", msg); }
            return;
        }
    };

    let server = match ConfigManager::load_server(&server_id) {
        Ok(s) => s,
        Err(e) => {
            if args.json { print_error(e.code(), &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    let base_path = match &project.base_path {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            let msg = format!("Base path not configured for project '{}'", args.project_id);
            if args.json { print_error("BASE_PATH_NOT_CONFIGURED", &msg); }
            else { eprintln!("Error: {}", msg); }
            return;
        }
    };

    let client = match homeboy_core::ssh::SshClient::from_server(&server, &server_id) {
        Ok(c) => c,
        Err(e) => {
            if args.json { print_error("SSH_ERROR", &e.to_string()); }
            else { eprintln!("Error: {}", e); }
            return;
        }
    };

    // Load components
    let all_components = load_components(&project.component_ids);

    if all_components.is_empty() {
        let msg = format!("No components configured for project '{}'", args.project_id);
        if args.json { print_error("NO_COMPONENTS", &msg); }
        else { eprintln!("Error: {}", msg); }
        return;
    }

    // Determine which components to deploy
    let components_to_deploy = if args.all {
        all_components.clone()
    } else if !args.component_ids.is_empty() {
        all_components
            .iter()
            .filter(|c| args.component_ids.contains(&c.id))
            .cloned()
            .collect()
    } else if args.outdated {
        all_components.clone() // TODO: Filter by version comparison
    } else {
        let msg = "No components specified. Use component IDs, --all, or --outdated";
        if args.json { print_error("NO_COMPONENTS_SPECIFIED", msg); }
        else { eprintln!("Error: {}", msg); }
        return;
    };

    if components_to_deploy.is_empty() {
        if args.json {
            print_success(DeployResult {
                components: vec![],
                summary: DeploySummary { succeeded: 0, failed: 0, skipped: 0 },
            });
        } else {
            println!("No components to deploy");
        }
        return;
    }

    // Fetch local versions
    let local_versions: HashMap<String, String> = components_to_deploy
        .iter()
        .filter_map(|c| fetch_local_version(c).map(|v| (c.id.clone(), v)))
        .collect();

    // Fetch remote versions for dry-run or outdated filtering
    let remote_versions = if args.dry_run || args.outdated {
        fetch_remote_versions(&components_to_deploy, &server, &base_path, &client)
    } else {
        HashMap::new()
    };

    // Build if requested
    if args.build {
        for component in &components_to_deploy {
            if let Some(ref build_cmd) = component.build_command {
                if !args.json {
                    println!("Building {}...", component.name);
                }

                if !args.dry_run {
                    let status = Command::new("sh")
                        .args(["-c", build_cmd])
                        .current_dir(&component.local_path)
                        .status();

                    if let Ok(s) = status {
                        if !s.success() && !args.json {
                            eprintln!("Warning: Build failed for {}", component.name);
                        }
                    }
                }
            }
        }
    }

    // Deploy
    let mut results = Vec::new();
    let mut succeeded = 0u32;
    let mut failed = 0u32;

    for component in &components_to_deploy {
        if args.dry_run {
            if !args.json {
                println!("Would deploy: {} -> {}/{}", component.name, base_path, component.remote_path);
            }
            results.push(ComponentResult {
                id: component.id.clone(),
                name: component.name.clone(),
                status: "would_deploy".to_string(),
                duration: None,
                local_version: local_versions.get(&component.id).cloned(),
                remote_version: remote_versions.get(&component.id).cloned(),
                error: None,
            });
            succeeded += 1;
            continue;
        }

        let artifact_path = &component.build_artifact;
        if !Path::new(artifact_path).exists() {
            if !args.json {
                eprintln!("Artifact not found: {}", artifact_path);
            }
            results.push(ComponentResult {
                id: component.id.clone(),
                name: component.name.clone(),
                status: "failed".to_string(),
                duration: None,
                local_version: local_versions.get(&component.id).cloned(),
                remote_version: None,
                error: Some(format!("Artifact not found: {}", artifact_path)),
            });
            failed += 1;
            continue;
        }

        if !args.json {
            println!("Deploying {}...", component.name);
        }

        let remote_path = if component.remote_path.starts_with('/') {
            component.remote_path.clone()
        } else {
            format!("{}/{}", base_path, component.remote_path)
        };

        // Use scp to upload
        let mut scp_args = Vec::new();

        if let Some(identity_file) = &client.identity_file {
            scp_args.push("-i".to_string());
            scp_args.push(identity_file.clone());
        }

        if server.port != 22 {
            scp_args.push("-P".to_string());
            scp_args.push(server.port.to_string());
        }

        scp_args.push(artifact_path.to_string());
        scp_args.push(format!("{}@{}:{}", server.user, server.host, remote_path));

        let scp_status = Command::new("scp").args(&scp_args).output();

        match scp_status {
            Ok(output) if output.status.success() => {
                // Unzip if it's a zip file
                if artifact_path.ends_with(".zip") {
                    let remote_dir = Path::new(&remote_path)
                        .parent()
                        .map(|p| p.to_string_lossy().to_string())
                        .unwrap_or_else(|| base_path.clone());

                    let unzip_cmd = format!(
                        "cd '{}' && unzip -o '{}' && rm '{}'",
                        remote_dir,
                        remote_path,
                        remote_path
                    );

                    let _ = client.execute(&unzip_cmd);
                }

                if !args.json {
                    println!("  ✓ {}", component.name);
                }
                results.push(ComponentResult {
                    id: component.id.clone(),
                    name: component.name.clone(),
                    status: "deployed".to_string(),
                    duration: None,
                    local_version: local_versions.get(&component.id).cloned(),
                    remote_version: local_versions.get(&component.id).cloned(), // After deploy, remote = local
                    error: None,
                });
                succeeded += 1;
            }
            Ok(output) => {
                let error = String::from_utf8_lossy(&output.stderr).to_string();
                if !args.json {
                    eprintln!("  ✗ {} - {}", component.name, error);
                }
                results.push(ComponentResult {
                    id: component.id.clone(),
                    name: component.name.clone(),
                    status: "failed".to_string(),
                    duration: None,
                    local_version: local_versions.get(&component.id).cloned(),
                    remote_version: None,
                    error: Some(error),
                });
                failed += 1;
            }
            Err(e) => {
                if !args.json {
                    eprintln!("  ✗ {} - {}", component.name, e);
                }
                results.push(ComponentResult {
                    id: component.id.clone(),
                    name: component.name.clone(),
                    status: "failed".to_string(),
                    duration: None,
                    local_version: local_versions.get(&component.id).cloned(),
                    remote_version: None,
                    error: Some(e.to_string()),
                });
                failed += 1;
            }
        }
    }

    if args.json {
        print_success(DeployResult {
            components: results,
            summary: DeploySummary { succeeded, failed, skipped: 0 },
        });
    } else if !args.dry_run {
        println!();
        println!("Deployment complete: {} succeeded, {} failed", succeeded, failed);
    }
}

#[derive(Clone)]
struct Component {
    id: String,
    name: String,
    local_path: String,
    remote_path: String,
    build_artifact: String,
    build_command: Option<String>,
    version_file: Option<String>,
    version_pattern: Option<String>,
}

fn load_components(component_ids: &[String]) -> Vec<Component> {
    let mut components = Vec::new();

    for id in component_ids {
        let path = AppPaths::component(id);
        if let Ok(content) = fs::read_to_string(&path) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&content) {
                components.push(Component {
                    id: config["id"].as_str().unwrap_or(id).to_string(),
                    name: config["name"].as_str().unwrap_or(id).to_string(),
                    local_path: config["localPath"].as_str().unwrap_or("").to_string(),
                    remote_path: config["remotePath"].as_str().unwrap_or("").to_string(),
                    build_artifact: config["buildArtifact"].as_str().map(|s| {
                        let local = config["localPath"].as_str().unwrap_or("");
                        if s.starts_with('/') {
                            s.to_string()
                        } else {
                            format!("{}/{}", local, s)
                        }
                    }).unwrap_or_default(),
                    build_command: config["buildCommand"].as_str().map(|s| s.to_string()),
                    version_file: config["versionFile"].as_str().map(|s| s.to_string()),
                    version_pattern: config["versionPattern"].as_str().map(|s| s.to_string()),
                });
            }
        }
    }

    components
}

// MARK: - Version Parsing

fn parse_component_version(content: &str, pattern: Option<&str>) -> Option<String> {
    let default_pattern = r"Version:\s*(\d+\.\d+\.\d+)";
    let pattern_str = match pattern {
        Some(p) => p.replace("\\\\", "\\"), // Handle double-escaped patterns from JSON
        None => default_pattern.to_string(),
    };
    parse_version(content, &pattern_str)
}

fn fetch_local_version(component: &Component) -> Option<String> {
    let version_file = component.version_file.as_ref()?;
    let path = format!("{}/{}", component.local_path, version_file);
    let content = fs::read_to_string(&path).ok()?;
    parse_component_version(&content, component.version_pattern.as_deref())
}

fn fetch_remote_versions(
    components: &[Component],
    _server: &ServerConfig,
    base_path: &str,
    client: &homeboy_core::ssh::SshClient,
) -> HashMap<String, String> {
    let mut versions = HashMap::new();

    for component in components {
        if let Some(version_file) = &component.version_file {
            let remote_path = if component.remote_path.starts_with('/') {
                format!("{}/{}", component.remote_path, version_file)
            } else {
                format!("{}/{}/{}", base_path, component.remote_path, version_file)
            };

            let output = client.execute(&format!("cat '{}' 2>/dev/null", remote_path));

            if output.success {
                if let Some(version) = parse_component_version(&output.stdout, component.version_pattern.as_deref()) {
                    versions.insert(component.id.clone(), version);
                }
            }
        }
    }

    versions
}
