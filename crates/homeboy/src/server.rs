use crate::error::{Error, Result};
use crate::files::{self, FileSystem};
use crate::json;
use crate::paths;
use serde::{Deserialize, Serialize};

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
    let content = files::local().read(&path)?;
    json::from_str(&content)
}

pub fn list() -> Result<Vec<Server>> {
    let dir = paths::servers()?;
    let entries = files::local().list(&dir)?;

    let mut servers: Vec<Server> = entries
        .into_iter()
        .filter(|e| e.is_json() && !e.is_dir)
        .filter_map(|e| {
            let content = files::local().read(&e.path).ok()?;
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
    files::ensure_app_dirs()?;
    let content = json::to_string_pretty(server)?;
    files::local().write(&path, &content)?;
    Ok(())
}

pub fn delete(id: &str) -> Result<()> {
    let path = paths::server(id)?;
    if !path.exists() {
        return Err(Error::server_not_found(id.to_string()));
    }
    files::local().delete(&path)?;
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

    files::ensure_app_dirs()?;
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
