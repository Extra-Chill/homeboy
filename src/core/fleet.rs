use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::paths;
use crate::project;
use crate::server;
use crate::ssh::SshClient;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Fleet {
    #[serde(skip_deserializing, default)]
    pub id: String,

    #[serde(default)]
    pub project_ids: Vec<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Fleet {
    pub fn new(id: String, project_ids: Vec<String>) -> Self {
        Self {
            id,
            project_ids,
            description: None,
        }
    }

    /// Returns project IDs that actually exist
    pub fn valid_project_ids(&self) -> Vec<String> {
        self.project_ids
            .iter()
            .filter(|id| project::exists(id))
            .cloned()
            .collect()
    }
}

impl ConfigEntity for Fleet {
    const ENTITY_TYPE: &'static str = "fleet";
    const DIR_NAME: &'static str = "fleets";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::fleet_not_found(id, suggestions)
    }
}

// ============================================================================
// Core CRUD - Thin wrappers around config module
// ============================================================================

pub fn load(id: &str) -> Result<Fleet> {
    config::load::<Fleet>(id)
}

pub fn list() -> Result<Vec<Fleet>> {
    config::list::<Fleet>()
}

pub fn list_ids() -> Result<Vec<String>> {
    config::list_ids::<Fleet>()
}

pub fn save(fleet: &Fleet) -> Result<()> {
    config::save(fleet)
}

pub fn delete(id: &str) -> Result<()> {
    config::delete::<Fleet>(id)
}

pub fn exists(id: &str) -> bool {
    config::exists::<Fleet>(id)
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<Fleet>(id, json_spec, replace_fields)
}

pub fn remove_from_json(id: Option<&str>, json_spec: &str) -> Result<RemoveResult> {
    config::remove_from_json::<Fleet>(id, json_spec)
}

pub fn create(json_spec: &str, skip_existing: bool) -> Result<CreateOutput<Fleet>> {
    config::create::<Fleet>(json_spec, skip_existing)
}

// ============================================================================
// Operations
// ============================================================================

/// Add a project to a fleet
pub fn add_project(fleet_id: &str, project_id: &str) -> Result<Fleet> {
    let mut fleet = load(fleet_id)?;

    // Validate project exists
    if !project::exists(project_id) {
        let suggestions = config::find_similar_ids::<crate::project::Project>(project_id);
        return Err(Error::project_not_found(project_id, suggestions));
    }

    // Don't add duplicates
    if !fleet.project_ids.contains(&project_id.to_string()) {
        fleet.project_ids.push(project_id.to_string());
        save(&fleet)?;
    }

    Ok(fleet)
}

/// Remove a project from a fleet
pub fn remove_project(fleet_id: &str, project_id: &str) -> Result<Fleet> {
    let mut fleet = load(fleet_id)?;

    fleet.project_ids.retain(|id| id != project_id);
    save(&fleet)?;

    Ok(fleet)
}

/// Rename a fleet
pub fn rename(id: &str, new_id: &str) -> Result<Fleet> {
    let new_id = new_id.to_lowercase();
    config::rename::<Fleet>(id, &new_id)?;
    load(&new_id)
}

/// Get all projects in a fleet with full project data
pub fn get_projects(fleet_id: &str) -> Result<Vec<crate::project::Project>> {
    let fleet = load(fleet_id)?;
    let mut projects = Vec::new();

    for project_id in &fleet.project_ids {
        if let Ok(project) = project::load(project_id) {
            projects.push(project);
        }
    }

    Ok(projects)
}

/// Get component usage across a fleet (component_id -> Vec<project_id>)
pub fn component_usage(fleet_id: &str) -> Result<std::collections::HashMap<String, Vec<String>>> {
    let fleet = load(fleet_id)?;
    let mut usage: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();

    for project_id in &fleet.project_ids {
        if let Ok(project) = project::load(project_id) {
            for component_id in &project.component_ids {
                usage
                    .entry(component_id.clone())
                    .or_default()
                    .push(project_id.clone());
            }
        }
    }

    Ok(usage)
}

// ============================================================================
// Fleet Sync
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSyncManifest {
    pub leader: String,
    pub categories: HashMap<String, FleetSyncCategory>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSyncCategory {
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_paths: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetSyncResult {
    pub leader_project_id: String,
    pub dry_run: bool,
    pub projects: Vec<FleetProjectSyncResult>,
    pub summary: FleetSyncSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetProjectSyncResult {
    pub project_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub categories: Vec<FleetSyncCategoryResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FleetSyncCategoryResult {
    pub category: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub files_synced: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct FleetSyncSummary {
    pub total_projects: u32,
    pub projects_synced: u32,
    pub projects_failed: u32,
    pub projects_skipped: u32,
    pub total_categories: u32,
    pub categories_synced: u32,
    pub categories_failed: u32,
}

/// Load fleet sync manifest from homeboy config directory
fn load_sync_manifest() -> Result<FleetSyncManifest> {
    let manifest_path = paths::homeboy()?.join("fleet-sync.json");

    if !manifest_path.exists() {
        return Err(Error::validation_missing_argument(vec![
            "fleet-sync.json".to_string()
        ]));
    }

    let content = std::fs::read_to_string(&manifest_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read {}", manifest_path.display())),
        )
    })?;

    serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_argument(
            "fleet-sync.json",
            format!("Invalid JSON: {}", e),
            None,
            None,
        )
    })
}

/// Sync OpenClaw agent configurations across fleet servers
pub fn sync(
    fleet_id: &str,
    category_filter: Option<Vec<String>>,
    dry_run: bool,
    leader_override: Option<String>,
) -> Result<FleetSyncResult> {
    let fleet = load(fleet_id)?;
    let manifest = load_sync_manifest()?;

    // Determine leader
    let leader_project_id = leader_override.unwrap_or(manifest.leader.clone());

    // Validate leader exists in fleet
    if !fleet.project_ids.contains(&leader_project_id) {
        return Err(Error::validation_invalid_argument(
            "leader",
            "Leader project not found in fleet",
            Some(leader_project_id),
            Some(fleet.project_ids.clone()),
        ));
    }

    eprintln!("[fleet sync] Leader: {}", leader_project_id);
    if dry_run {
        eprintln!("[fleet sync] DRY RUN MODE");
    }

    // Filter categories
    let active_categories: Vec<(String, FleetSyncCategory)> = manifest
        .categories
        .iter()
        .filter(|(name, cat)| {
            if !cat.enabled {
                return false;
            }
            if let Some(ref filter) = category_filter {
                filter.contains(name)
            } else {
                true
            }
        })
        .map(|(name, cat)| (name.clone(), cat.clone()))
        .collect();

    if active_categories.is_empty() {
        return Err(Error::validation_invalid_argument(
            "categories",
            "No enabled categories to sync",
            None,
            None,
        ));
    }

    eprintln!(
        "[fleet sync] Categories: {}",
        active_categories
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let mut project_results = Vec::new();
    let mut summary = FleetSyncSummary {
        total_projects: 0,
        total_categories: active_categories.len() as u32,
        ..Default::default()
    };

    for project_id in &fleet.project_ids {
        // Skip leader
        if project_id == &leader_project_id {
            eprintln!("[fleet sync] Skipping leader: {}", project_id);
            summary.projects_skipped += 1;
            continue;
        }

        summary.total_projects += 1;
        eprintln!("[fleet sync] Syncing to project: {}", project_id);

        match sync_project(
            &leader_project_id,
            project_id,
            &active_categories,
            dry_run,
            &mut summary,
        ) {
            Ok(result) => {
                if result.status == "synced" {
                    summary.projects_synced += 1;
                } else {
                    summary.projects_failed += 1;
                }
                project_results.push(result);
            }
            Err(e) => {
                summary.projects_failed += 1;
                let proj = project::load(project_id).ok();
                project_results.push(FleetProjectSyncResult {
                    project_id: project_id.clone(),
                    server_id: proj.and_then(|p| p.server_id),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    categories: vec![],
                });
            }
        }
    }

    Ok(FleetSyncResult {
        leader_project_id,
        dry_run,
        projects: project_results,
        summary,
    })
}

fn sync_project(
    leader_project_id: &str,
    target_project_id: &str,
    categories: &[(String, FleetSyncCategory)],
    dry_run: bool,
    summary: &mut FleetSyncSummary,
) -> Result<FleetProjectSyncResult> {
    let target_project = project::load(target_project_id)?;

    let server_id = target_project.server_id.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "project.server_id",
            "Target project has no server configured",
            Some(target_project_id.to_string()),
            None,
        )
    })?;

    let server = server::load(server_id)?;
    let ssh_client = SshClient::from_server(&server, server_id)?;

    // Auto-detect openclaw home on remote server
    let openclaw_home = detect_openclaw_home(&ssh_client)?;
    eprintln!(
        "[fleet sync]   OpenClaw home on {}: {}",
        target_project_id, openclaw_home
    );

    let mut category_results = Vec::new();

    for (category_name, category_config) in categories {
        eprintln!(
            "[fleet sync]   Category: {} {}",
            category_name,
            if dry_run { "(dry-run)" } else { "" }
        );

        let result = sync_category(
            leader_project_id,
            &ssh_client,
            &openclaw_home,
            category_name,
            category_config,
            dry_run,
        );

        match result {
            Ok(cat_result) => {
                if cat_result.status == "synced" {
                    summary.categories_synced += 1;
                } else if cat_result.status == "failed" {
                    summary.categories_failed += 1;
                }
                category_results.push(cat_result);
            }
            Err(e) => {
                summary.categories_failed += 1;
                category_results.push(FleetSyncCategoryResult {
                    category: category_name.clone(),
                    status: "failed".to_string(),
                    error: Some(e.to_string()),
                    files_synced: vec![],
                });
            }
        }
    }

    // Fix ownership if openclaw runs as non-root user
    if !dry_run && !openclaw_home.starts_with("/root/") {
        let user_home = std::path::Path::new(&openclaw_home)
            .parent()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        if let Some(user) = user_home.strip_prefix("/home/") {
            eprintln!("[fleet sync]   Fixing ownership for user: {}", user);
            ssh_client.execute(&format!("chown -R {}:{} {} {}", user, user, user_home, openclaw_home));
        }
    }

    Ok(FleetProjectSyncResult {
        project_id: target_project_id.to_string(),
        server_id: Some(server_id.clone()),
        status: "synced".to_string(),
        error: None,
        categories: category_results,
    })
}

fn detect_openclaw_home(ssh_client: &SshClient) -> Result<String> {
    let output = ssh_client
        .execute("find /root/.openclaw /home/*/.openclaw -maxdepth 1 -type f -name 'openclaw.json' 2>/dev/null | head -1");

    if !output.success || output.stdout.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "openclaw_home",
            "Could not find openclaw.json on remote server",
            None,
            None,
        ));
    }

    let openclaw_json_path = output.stdout.trim();
    let openclaw_home = std::path::Path::new(openclaw_json_path)
        .parent()
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "Invalid openclaw.json path: {}",
                openclaw_json_path
            ))
        })?
        .to_string_lossy()
        .to_string();

    Ok(openclaw_home)
}

fn sync_category(
    _leader_project_id: &str,
    ssh_client: &SshClient,
    openclaw_home: &str,
    category_name: &str,
    category_config: &FleetSyncCategory,
    dry_run: bool,
) -> Result<FleetSyncCategoryResult> {
    match category_name {
        "openclaw-config" => {
            sync_openclaw_config(ssh_client, openclaw_home, category_config, dry_run)
        }
        "opencode-config" => {
            sync_opencode_config(ssh_client, openclaw_home, category_config, dry_run)
        }
        "opencode-auth" => sync_opencode_auth(ssh_client, openclaw_home, category_config, dry_run),
        "skills" => sync_skills(ssh_client, openclaw_home, category_config, dry_run),
        "workspace-files" => {
            sync_workspace_files(ssh_client, openclaw_home, category_config, dry_run)
        }
        _ => Err(Error::validation_invalid_argument(
            "category",
            format!("Unknown category: {}", category_name),
            Some(category_name.to_string()),
            Some(vec![
                "openclaw-config".to_string(),
                "opencode-config".to_string(),
                "opencode-auth".to_string(),
                "skills".to_string(),
                "workspace-files".to_string(),
            ]),
        )),
    }
}

fn sync_openclaw_config(
    ssh_client: &SshClient,
    openclaw_home: &str,
    category_config: &FleetSyncCategory,
    dry_run: bool,
) -> Result<FleetSyncCategoryResult> {
    let remote_config_path = format!("{}/openclaw.json", openclaw_home);

    if dry_run {
        return Ok(FleetSyncCategoryResult {
            category: "openclaw-config".to_string(),
            status: "synced".to_string(),
            error: None,
            files_synced: vec![remote_config_path],
        });
    }

    // Read local openclaw.json
    let local_openclaw_path = shellexpand::tilde("~/.openclaw/openclaw.json").to_string();
    let local_content = std::fs::read_to_string(&local_openclaw_path).map_err(|e| {
        Error::internal_io(e.to_string(), Some("read local openclaw.json".to_string()))
    })?;

    let local_config: serde_json::Value = serde_json::from_str(&local_content).map_err(|e| {
        Error::validation_invalid_argument(
            "openclaw.json",
            format!("Invalid JSON: {}", e),
            None,
            None,
        )
    })?;

    // Read remote openclaw.json
    let output = ssh_client.execute(&format!("cat {}", remote_config_path));
    if !output.success {
        return Err(Error::validation_invalid_argument(
            "openclaw.json",
            "Failed to read remote openclaw.json",
            None,
            None,
        ));
    }

    let mut remote_config: serde_json::Value =
        serde_json::from_str(&output.stdout).map_err(|e| {
            Error::validation_invalid_argument(
                "openclaw.json",
                format!("Invalid remote JSON: {}", e),
                None,
                None,
            )
        })?;

    // Merge specified paths
    if let Some(merge_paths) = &category_config.merge_paths {
        for path in merge_paths {
            merge_json_path(&local_config, &mut remote_config, path)?;
        }
    }

    // Write back to remote
    let merged_json = serde_json::to_string_pretty(&remote_config).map_err(|e| {
        Error::internal_unexpected(format!("Failed to serialize merged config: {}", e))
    })?;

    // Create temp file and upload
    let temp_path = std::env::temp_dir().join(format!("openclaw-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&temp_path, &merged_json)
        .map_err(|e| Error::internal_io(e.to_string(), Some("write temp config".to_string())))?;

    let upload_result = ssh_client.upload_file(temp_path.to_str().unwrap(), &remote_config_path);

    // Cleanup temp file
    let _ = std::fs::remove_file(&temp_path);

    if !upload_result.success {
        return Err(Error::validation_invalid_argument(
            "upload",
            "Failed to upload merged config",
            None,
            None,
        ));
    }

    Ok(FleetSyncCategoryResult {
        category: "openclaw-config".to_string(),
        status: "synced".to_string(),
        error: None,
        files_synced: vec![remote_config_path],
    })
}

fn merge_json_path(
    source: &serde_json::Value,
    target: &mut serde_json::Value,
    path: &str,
) -> Result<()> {
    let parts: Vec<&str> = path.split('.').collect();

    // Navigate to source value
    let mut source_val = source;
    for part in &parts {
        source_val = source_val.get(part).ok_or_else(|| {
            Error::validation_invalid_argument(
                "merge_path",
                format!("Path not found in source: {}", path),
                Some(path.to_string()),
                None,
            )
        })?;
    }

    // Navigate to target location and set value
    let mut current = target;
    for (i, part) in parts.iter().enumerate() {
        if i == parts.len() - 1 {
            // Last part - set the value
            if let Some(obj) = current.as_object_mut() {
                obj.insert((*part).to_string(), source_val.clone());
            }
        } else {
            // Navigate deeper
            if !current.get(part).is_some() {
                // Create missing object
                if let Some(obj) = current.as_object_mut() {
                    obj.insert((*part).to_string(), serde_json::json!({}));
                }
            }
            current = current.get_mut(part).ok_or_else(|| {
                Error::internal_unexpected(format!("Failed to navigate path: {}", path))
            })?;
        }
    }

    Ok(())
}

fn sync_opencode_config(
    ssh_client: &SshClient,
    openclaw_home: &str,
    category_config: &FleetSyncCategory,
    dry_run: bool,
) -> Result<FleetSyncCategoryResult> {
    let files = category_config.files.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "opencode-config",
            "No files specified for opencode-config category",
            None,
            None,
        )
    })?;

    // Derive remote user home from openclaw_home (e.g. /home/openclaw/.openclaw -> /home/openclaw)
    let remote_user_home = std::path::Path::new(openclaw_home)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/root".to_string());
    let local_home = shellexpand::tilde("~").to_string();

    if dry_run {
        return Ok(FleetSyncCategoryResult {
            category: "opencode-config".to_string(),
            status: "synced".to_string(),
            error: None,
            files_synced: files.clone(),
        });
    }

    let mut synced_files = Vec::new();

    for file_path in files {
        let local_path = format!("{}/{}", local_home, file_path);
        let remote_path = format!("{}/{}", remote_user_home, file_path);

        // Ensure remote parent dir exists
        let remote_dir = std::path::Path::new(&remote_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("");
        if !remote_dir.is_empty() {
            ssh_client.execute(&format!("mkdir -p {}", remote_dir));
        }

        if file_path.ends_with('/') {
            // Directory - use tar
            sync_directory_via_tar(ssh_client, local_path.trim_end_matches('/'), &remote_path)?;
        } else {
            // Single file
            sync_file(ssh_client, &local_path, &remote_path)?;
        }

        synced_files.push(remote_path);
    }

    Ok(FleetSyncCategoryResult {
        category: "opencode-config".to_string(),
        status: "synced".to_string(),
        error: None,
        files_synced: synced_files,
    })
}

fn sync_opencode_auth(
    ssh_client: &SshClient,
    openclaw_home: &str,
    category_config: &FleetSyncCategory,
    dry_run: bool,
) -> Result<FleetSyncCategoryResult> {
    let files = category_config.files.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "opencode-auth",
            "No files specified for opencode-auth category",
            None,
            None,
        )
    })?;

    if dry_run {
        return Ok(FleetSyncCategoryResult {
            category: "opencode-auth".to_string(),
            status: "synced".to_string(),
            error: None,
            files_synced: files.clone(),
        });
    }

    let remote_user_home = std::path::Path::new(openclaw_home)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| "/root".to_string());
    let local_home = shellexpand::tilde("~").to_string();

    let mut synced_files = Vec::new();

    for file_path in files {
        let local_path = format!("{}/{}", local_home, file_path);
        let remote_path = format!("{}/{}", remote_user_home, file_path);

        // Ensure remote parent dir exists
        let remote_dir = std::path::Path::new(&remote_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("");

        if !remote_dir.is_empty() {
            ssh_client.execute(&format!("mkdir -p {}", remote_dir));
        }

        sync_file(ssh_client, &local_path, &remote_path)?;
        synced_files.push(remote_path);
    }

    Ok(FleetSyncCategoryResult {
        category: "opencode-auth".to_string(),
        status: "synced".to_string(),
        error: None,
        files_synced: synced_files,
    })
}

fn sync_skills(
    ssh_client: &SshClient,
    openclaw_home: &str,
    category_config: &FleetSyncCategory,
    dry_run: bool,
) -> Result<FleetSyncCategoryResult> {
    let items = category_config.items.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "skills",
            "No items specified for skills category",
            None,
            None,
        )
    })?;

    if dry_run {
        return Ok(FleetSyncCategoryResult {
            category: "skills".to_string(),
            status: "synced".to_string(),
            error: None,
            files_synced: items.clone(),
        });
    }

    let mut synced_files = Vec::new();

    let local_workspace = shellexpand::tilde("~/.openclaw/workspace").to_string();
    let remote_workspace = format!("{}/workspace", openclaw_home);

    for item_path in items {
        let local_path = format!("{}/{}", local_workspace, item_path.trim_end_matches('/'));
        let remote_path = format!("{}/{}", remote_workspace, item_path.trim_end_matches('/'));

        // Ensure remote parent dir exists
        let remote_parent = std::path::Path::new(&remote_path)
            .parent()
            .and_then(|p| p.to_str())
            .unwrap_or("");
        if !remote_parent.is_empty() {
            ssh_client.execute(&format!("mkdir -p {}", remote_parent));
        }

        sync_directory_via_tar(ssh_client, &local_path, &remote_path)?;
        synced_files.push(remote_path);
    }

    Ok(FleetSyncCategoryResult {
        category: "skills".to_string(),
        status: "synced".to_string(),
        error: None,
        files_synced: synced_files,
    })
}

fn sync_workspace_files(
    ssh_client: &SshClient,
    openclaw_home: &str,
    category_config: &FleetSyncCategory,
    dry_run: bool,
) -> Result<FleetSyncCategoryResult> {
    let items = category_config.items.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace-files",
            "No items specified for workspace-files category",
            None,
            None,
        )
    })?;

    if dry_run {
        return Ok(FleetSyncCategoryResult {
            category: "workspace-files".to_string(),
            status: "synced".to_string(),
            error: None,
            files_synced: items.clone(),
        });
    }

    let mut synced_files = Vec::new();

    let local_workspace = shellexpand::tilde("~/.openclaw/workspace").to_string();
    let remote_workspace = format!("{}/workspace", openclaw_home);

    for item_path in items {
        let local_path = format!("{}/{}", local_workspace, item_path);
        let remote_path = format!("{}/{}", remote_workspace, item_path);

        sync_file(ssh_client, &local_path, &remote_path)?;
        synced_files.push(remote_path);
    }

    Ok(FleetSyncCategoryResult {
        category: "workspace-files".to_string(),
        status: "synced".to_string(),
        error: None,
        files_synced: synced_files,
    })
}

fn sync_file(ssh_client: &SshClient, local_path: &str, remote_path: &str) -> Result<()> {
    if !std::path::Path::new(local_path).exists() {
        return Err(Error::validation_invalid_argument(
            "local_path",
            format!("Local file not found: {}", local_path),
            Some(local_path.to_string()),
            None,
        ));
    }

    let output = ssh_client.upload_file(local_path, remote_path);

    if !output.success {
        return Err(Error::validation_invalid_argument(
            "upload",
            format!("Failed to upload file: {}", output.stderr),
            Some(local_path.to_string()),
            None,
        ));
    }

    Ok(())
}

fn sync_directory_via_tar(ssh_client: &SshClient, local_dir: &str, remote_dir: &str) -> Result<()> {
    if !std::path::Path::new(local_dir).exists() {
        return Err(Error::validation_invalid_argument(
            "local_dir",
            format!("Local directory not found: {}", local_dir),
            Some(local_dir.to_string()),
            None,
        ));
    }

    // Ensure remote parent exists
    let remote_parent = std::path::Path::new(remote_dir)
        .parent()
        .and_then(|p| p.to_str())
        .unwrap_or("");

    if !remote_parent.is_empty() {
        ssh_client.execute(&format!("mkdir -p {}", remote_parent));
    }

    // Create tar on local, stream to remote and extract
    let command = format!(
        "tar -czf - -C {} . | ssh_placeholder 'mkdir -p {} && tar -xzf - -C {}'",
        crate::utils::shell::quote_path(local_dir),
        crate::utils::shell::quote_path(remote_dir),
        crate::utils::shell::quote_path(remote_dir)
    );

    // Use the actual SSH connection args
    let ssh_args = build_ssh_connection_string(ssh_client);
    let full_command = command.replace("ssh_placeholder", &ssh_args);

    let output = std::process::Command::new("sh")
        .args(["-c", &full_command])
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), Some("tar directory sync".to_string())))?;

    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "sync_directory",
            format!(
                "Failed to sync directory: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
            Some(local_dir.to_string()),
            None,
        ));
    }

    Ok(())
}

fn build_ssh_connection_string(ssh_client: &SshClient) -> String {
    let mut args = vec!["ssh".to_string()];

    if let Some(identity_file) = &ssh_client.identity_file {
        args.push("-i".to_string());
        args.push(identity_file.clone());
    }

    if ssh_client.port != 22 {
        args.push("-p".to_string());
        args.push(ssh_client.port.to_string());
    }

    args.push(format!("{}@{}", ssh_client.user, ssh_client.host));

    args.join(" ")
}
