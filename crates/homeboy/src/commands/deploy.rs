use clap::Args;
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use homeboy_core::config::{ConfigManager, AppPaths, ServerConfig};
use homeboy_core::output::{print_success, print_error};

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

    let key_path = AppPaths::key(&server_id);
    if !key_path.exists() {
        let msg = "SSH key not found for server";
        if args.json { print_error("SSH_KEY_NOT_FOUND", msg); }
        else { eprintln!("Error: {}", msg); }
        return;
    }

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
        fetch_remote_versions(&components_to_deploy, &server, &base_path, &key_path)
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
        let scp_status = Command::new("scp")
            .args([
                "-i", &key_path.to_string_lossy(),
                "-o", "StrictHostKeyChecking=no",
                artifact_path,
                &format!("{}@{}:{}", server.user, server.host, remote_path),
            ])
            .output();

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

                    let _ = Command::new("ssh")
                        .args([
                            "-i", &key_path.to_string_lossy(),
                            "-o", "StrictHostKeyChecking=no",
                            &format!("{}@{}", server.user, server.host),
                            &unzip_cmd,
                        ])
                        .output();
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

fn parse_version(content: &str, pattern: Option<&str>) -> Option<String> {
    let pattern_str = pattern.unwrap_or(r"Version:\s*(\d+\.\d+\.\d+)");
    let re = Regex::new(pattern_str).ok()?;
    re.captures(content)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

fn fetch_local_version(component: &Component) -> Option<String> {
    let version_file = component.version_file.as_ref()?;
    let path = format!("{}/{}", component.local_path, version_file);
    let content = fs::read_to_string(&path).ok()?;
    parse_version(&content, component.version_pattern.as_deref())
}

fn fetch_remote_versions(
    components: &[Component],
    server: &ServerConfig,
    base_path: &str,
    key_path: &Path,
) -> HashMap<String, String> {
    let mut versions = HashMap::new();

    for component in components {
        if let Some(version_file) = &component.version_file {
            let remote_path = if component.remote_path.starts_with('/') {
                format!("{}/{}", component.remote_path, version_file)
            } else {
                format!("{}/{}/{}", base_path, component.remote_path, version_file)
            };

            let output = Command::new("ssh")
                .args([
                    "-i", &key_path.to_string_lossy(),
                    "-o", "StrictHostKeyChecking=no",
                    "-o", "BatchMode=yes",
                    &format!("{}@{}", server.user, server.host),
                    &format!("cat '{}' 2>/dev/null", remote_path),
                ])
                .output();

            if let Ok(output) = output {
                if output.status.success() {
                    let content = String::from_utf8_lossy(&output.stdout);
                    if let Some(version) = parse_version(&content, component.version_pattern.as_deref()) {
                        versions.insert(component.id.clone(), version);
                    }
                }
            }
        }
    }

    versions
}
