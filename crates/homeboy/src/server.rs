use crate::error::{Error, Result};
use crate::local_files::{self, FileSystem};
use crate::json;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub id: String,
    pub name: String,
    pub host: String,
    pub user: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub identity_file: Option<String>,
}

fn default_port() -> u16 {
    22
}

impl Server {
    pub fn keychain_service_name(&self, prefix: &str) -> String {
        format!("{}.{}", prefix, self.id)
    }

    pub fn is_valid(&self) -> bool {
        !self.host.is_empty() && !self.user.is_empty()
    }

    pub fn generate_id(host: &str) -> String {
        format!("server-{}", host.replace('.', "-"))
    }
}

pub fn load(id: &str) -> Result<Server> {
    let path = paths::server(id)?;
    if !path.exists() {
        return Err(Error::server_not_found(id.to_string()));
    }
    let content = local_files::local().read(&path)?;
    json::from_str(&content)
}

pub fn list() -> Result<Vec<Server>> {
    let dir = paths::servers()?;
    let entries = local_files::local().list(&dir)?;

    let mut servers: Vec<Server> = entries
        .into_iter()
        .filter(|e| e.is_json() && !e.is_dir)
        .filter_map(|e| {
            let content = local_files::local().read(&e.path).ok()?;
            json::from_str(&content).ok()
        })
        .collect();
    servers.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(servers)
}

pub fn save(server: &Server) -> Result<()> {
    let expected_id = slugify_id(&server.name)?;
    if expected_id != server.id {
        return Err(Error::config_invalid_value(
            "server.id",
            Some(server.id.clone()),
            format!(
                "Server id '{}' must match slug(name) '{}'. Use rename to change.",
                server.id, expected_id
            ),
        ));
    }

    let path = paths::server(&server.id)?;
    local_files::ensure_app_dirs()?;
    let content = json::to_string_pretty(server)?;
    local_files::local().write(&path, &content)?;
    Ok(())
}

/// Merge JSON into server config. Accepts JSON string, @file, or - for stdin.
pub fn merge_from_json(id: &str, json_spec: &str) -> Result<json::MergeResult> {
    let mut server = load(id)?;
    let raw = json::read_json_spec_to_string(json_spec)?;
    let patch = json::from_str(&raw)?;
    let result = json::merge_config(&mut server, patch)?;
    save(&server)?;
    Ok(result)
}

pub fn delete(id: &str) -> Result<()> {
    let path = paths::server(id)?;
    if !path.exists() {
        return Err(Error::server_not_found(id.to_string()));
    }
    local_files::local().delete(&path)?;
    Ok(())
}

pub fn exists(id: &str) -> bool {
    paths::server(id).map(|p| p.exists()).unwrap_or(false)
}

pub fn slugify_id(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(Error::validation_invalid_argument(
            "name",
            "Name cannot be empty",
            None,
            None,
        ));
    }

    let mut out = String::new();
    let mut prev_was_dash = false;

    for ch in trimmed.chars() {
        let normalized = match ch {
            'a'..='z' | '0'..='9' => Some(ch),
            'A'..='Z' => Some(ch.to_ascii_lowercase()),
            _ if ch.is_whitespace() || ch == '_' || ch == '-' => Some('-'),
            _ => None,
        };

        if let Some(c) = normalized {
            if c == '-' {
                if out.is_empty() || prev_was_dash {
                    continue;
                }
                out.push('-');
                prev_was_dash = true;
            } else {
                out.push(c);
                prev_was_dash = false;
            }
        }
    }

    while out.ends_with('-') {
        out.pop();
    }

    if out.is_empty() {
        return Err(Error::validation_invalid_argument(
            "name",
            "Name must contain at least one letter or number",
            None,
            None,
        ));
    }

    Ok(out)
}

pub fn key_path(id: &str) -> Result<std::path::PathBuf> {
    paths::key(id)
}

// ============================================================================
// CLI Entry Points - Accept Option<T> and handle validation
// ============================================================================

#[derive(Debug, Clone)]
pub struct CreateResult {
    pub id: String,
    pub server: Server,
}

#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub id: String,
    pub server: Server,
    pub updated_fields: Vec<String>,
}

pub fn create_from_cli(
    name: Option<String>,
    host: Option<String>,
    user: Option<String>,
    port: Option<u16>,
) -> Result<CreateResult> {
    let name = name.ok_or_else(|| {
        Error::validation_invalid_argument("name", "Missing required argument: name", None, None)
    })?;

    let host = host.ok_or_else(|| {
        Error::validation_invalid_argument("host", "Missing required argument: host", None, None)
    })?;

    let user = user.ok_or_else(|| {
        Error::validation_invalid_argument("user", "Missing required argument: user", None, None)
    })?;

    let id = slugify_id(&name)?;
    let path = paths::server(&id)?;
    if path.exists() {
        return Err(Error::validation_invalid_argument(
            "server.name",
            format!("Server '{}' already exists", id),
            Some(id),
            None,
        ));
    }

    let server = Server {
        id: id.clone(),
        name,
        host,
        user,
        port: port.unwrap_or(22),
        identity_file: None,
    };

    save(&server)?;

    Ok(CreateResult { id, server })
}

pub fn update(
    server_id: &str,
    name: Option<String>,
    host: Option<String>,
    user: Option<String>,
    port: Option<u16>,
) -> Result<UpdateResult> {
    let mut server = load(server_id)?;
    let mut updated = Vec::new();

    if let Some(new_name) = name {
        let new_id = slugify_id(&new_name)?;
        if new_id != server_id {
            return Err(Error::validation_invalid_argument(
                "name",
                format!(
                    "Changing name would change id from '{}' to '{}'. Use rename command instead.",
                    server_id, new_id
                ),
                Some(new_name),
                None,
            ));
        }
        server.name = new_name;
        updated.push("name".to_string());
    }

    if let Some(new_host) = host {
        server.host = new_host;
        updated.push("host".to_string());
    }

    if let Some(new_user) = user {
        server.user = new_user;
        updated.push("user".to_string());
    }

    if let Some(new_port) = port {
        server.port = new_port;
        updated.push("port".to_string());
    }

    save(&server)?;

    Ok(UpdateResult {
        id: server_id.to_string(),
        server,
        updated_fields: updated,
    })
}

pub fn rename(id: &str, new_name: &str) -> Result<CreateResult> {
    let mut server = load(id)?;
    let new_id = slugify_id(new_name)?;

    if new_id == id {
        server.name = new_name.to_string();
        save(&server)?;
        return Ok(CreateResult {
            id: new_id,
            server,
        });
    }

    let old_path = paths::server(id)?;
    let new_path = paths::server(&new_id)?;

    if new_path.exists() {
        return Err(Error::validation_invalid_argument(
            "server.name",
            format!(
                "Cannot rename server '{}' to '{}': destination already exists",
                id, new_id
            ),
            Some(new_id),
            None,
        ));
    }

    server.id = new_id.clone();
    server.name = new_name.to_string();

    local_files::ensure_app_dirs()?;
    std::fs::rename(&old_path, &new_path).map_err(|e| {
        Error::internal_io(e.to_string(), Some("rename server".to_string()))
    })?;

    if let Err(error) = save(&server) {
        let _ = std::fs::rename(&new_path, &old_path);
        return Err(error);
    }

    Ok(CreateResult {
        id: new_id,
        server,
    })
}

pub fn delete_with_validation(id: &str, force: bool) -> Result<()> {
    if !exists(id) {
        return Err(Error::server_not_found(id.to_string()));
    }

    if !force {
        return Err(Error::validation_invalid_argument(
            "force",
            "Use --force to confirm deletion",
            Some(id.to_string()),
            None,
        ));
    }

    delete(id)
}

pub fn set_identity_file(id: &str, identity_file: Option<String>) -> Result<Server> {
    let mut server = load(id)?;
    server.identity_file = identity_file;
    save(&server)?;
    Ok(server)
}

// ============================================================================
// JSON Import
// ============================================================================

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSummary {
    pub created: u32,
    pub skipped: u32,
    pub errors: u32,
    pub items: Vec<CreateSummaryItem>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSummaryItem {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn create_from_json(spec: &str, skip_existing: bool) -> Result<CreateSummary> {
    let value: serde_json::Value = json::from_str(spec)?;

    let items: Vec<serde_json::Value> = if value.is_array() {
        value.as_array().unwrap().clone()
    } else {
        vec![value]
    };

    let mut summary = CreateSummary {
        created: 0,
        skipped: 0,
        errors: 0,
        items: Vec::new(),
    };

    for item in items {
        let server: Server = match serde_json::from_value(item.clone()) {
            Ok(s) => s,
            Err(e) => {
                let id = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|n| slugify_id(n).unwrap_or_else(|_| "unknown".to_string()))
                    .unwrap_or_else(|| "unknown".to_string());

                summary.errors += 1;
                summary.items.push(CreateSummaryItem {
                    id,
                    status: "error".to_string(),
                    error: Some(format!("Parse error: {}", e)),
                });
                continue;
            }
        };

        let id = match slugify_id(&server.name) {
            Ok(id) => id,
            Err(e) => {
                summary.errors += 1;
                summary.items.push(CreateSummaryItem {
                    id: "unknown".to_string(),
                    status: "error".to_string(),
                    error: Some(e.message.clone()),
                });
                continue;
            }
        };

        if exists(&id) {
            if skip_existing {
                summary.skipped += 1;
                summary.items.push(CreateSummaryItem {
                    id,
                    status: "skipped".to_string(),
                    error: None,
                });
            } else {
                summary.errors += 1;
                summary.items.push(CreateSummaryItem {
                    id: id.clone(),
                    status: "error".to_string(),
                    error: Some(format!("Server '{}' already exists", id)),
                });
            }
            continue;
        }

        let server_with_id = Server {
            id: id.clone(),
            ..server
        };

        if let Err(e) = save(&server_with_id) {
            summary.errors += 1;
            summary.items.push(CreateSummaryItem {
                id,
                status: "error".to_string(),
                error: Some(e.message.clone()),
            });
            continue;
        }

        summary.created += 1;
        summary.items.push(CreateSummaryItem {
            id,
            status: "created".to_string(),
            error: None,
        });
    }

    Ok(summary)
}

// ============================================================================
// SSH Key Management
// ============================================================================

/// Result of generating an SSH key pair
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyGenerateResult {
    pub server: Server,
    pub public_key: String,
    pub identity_file: String,
}

/// Result of importing an SSH key
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyImportResult {
    pub server: Server,
    pub public_key: String,
    pub identity_file: String,
    pub imported_from: String,
}

/// Generate a new SSH key pair for a server.
pub fn generate_key(server_id: &str) -> Result<KeyGenerateResult> {
    load(server_id)?;

    let key_path = key_path(server_id)?;
    let key_path_str = key_path.to_string_lossy().to_string();

    if let Some(parent) = key_path.parent() {
        local_files::local().ensure_dir(parent)?;
    }

    let _ = std::fs::remove_file(&key_path);
    let _ = std::fs::remove_file(format!("{}.pub", key_path_str));

    let output = Command::new("ssh-keygen")
        .args([
            "-t", "rsa",
            "-b", "4096",
            "-f", &key_path_str,
            "-N", "",
            "-C", &format!("homeboy-{}", server_id),
        ])
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), Some("run ssh-keygen".to_string())))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::internal_unexpected(format!("ssh-keygen failed: {}", stderr)));
    }

    let server = set_identity_file(server_id, Some(key_path_str.clone()))?;

    let pub_key_path = format!("{}.pub", key_path_str);
    let public_key = local_files::local().read(std::path::Path::new(&pub_key_path))?;

    Ok(KeyGenerateResult {
        server,
        public_key: public_key.trim().to_string(),
        identity_file: key_path_str,
    })
}

/// Get the public key for a server.
pub fn get_public_key(server_id: &str) -> Result<String> {
    load(server_id)?;

    let key_path = key_path(server_id)?;
    let pub_key_path = format!("{}.pub", key_path.to_string_lossy());

    let public_key = std::fs::read_to_string(&pub_key_path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            Error::ssh_identity_file_not_found(server_id.to_string(), pub_key_path)
        } else {
            Error::internal_io(e.to_string(), Some("read ssh public key".to_string()))
        }
    })?;

    Ok(public_key.trim().to_string())
}

/// Import an existing SSH private key for a server.
pub fn import_key(server_id: &str, source_path: &str) -> Result<KeyImportResult> {
    load(server_id)?;

    let expanded_path = shellexpand::tilde(source_path).to_string();

    let private_key = std::fs::read_to_string(&expanded_path).map_err(|e| {
        Error::internal_io(e.to_string(), Some("read ssh private key".to_string()))
    })?;

    if !private_key.contains("-----BEGIN") || !private_key.contains("PRIVATE KEY-----") {
        return Err(Error::validation_invalid_argument(
            "privateKeyPath",
            "File doesn't appear to be a valid SSH private key",
            Some(server_id.to_string()),
            Some(vec![expanded_path.clone()]),
        ));
    }

    let output = Command::new("ssh-keygen")
        .args(["-y", "-f", &expanded_path])
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), Some("run ssh-keygen -y".to_string())))?;

    if !output.status.success() {
        return Err(Error::internal_unexpected(
            "Failed to derive public key from private key".to_string(),
        ));
    }

    let public_key = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let key_path = key_path(server_id)?;
    let key_path_str = key_path.to_string_lossy().to_string();

    if let Some(parent) = key_path.parent() {
        local_files::local().ensure_dir(parent)?;
    }

    std::fs::write(&key_path, &private_key).map_err(|e| {
        Error::internal_io(e.to_string(), Some("write ssh private key".to_string()))
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| Error::internal_io(e.to_string(), Some("set ssh key permissions".to_string())))?;
    }

    std::fs::write(format!("{}.pub", key_path_str), &public_key).map_err(|e| {
        Error::internal_io(e.to_string(), Some("write ssh public key".to_string()))
    })?;

    let server = set_identity_file(server_id, Some(key_path_str.clone()))?;

    Ok(KeyImportResult {
        server,
        public_key,
        identity_file: key_path_str,
        imported_from: expanded_path,
    })
}

/// Set identity file for a server by referencing an existing key path.
pub fn use_key(server_id: &str, key_path: &str) -> Result<Server> {
    let expanded_path = shellexpand::tilde(key_path).to_string();

    if !std::path::Path::new(&expanded_path).exists() {
        return Err(Error::ssh_identity_file_not_found(
            server_id.to_string(),
            expanded_path,
        ));
    }

    set_identity_file(server_id, Some(expanded_path))
}

/// Clear the identity file for a server.
pub fn unset_key(server_id: &str) -> Result<Server> {
    set_identity_file(server_id, None)
}
