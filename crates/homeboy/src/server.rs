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
