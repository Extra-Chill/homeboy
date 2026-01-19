use crate::config::{self, ConfigEntity};
use crate::error::{Error, Result};
use crate::local_files::{self, FileSystem};
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use crate::paths;
use crate::project;
use serde::{Deserialize, Serialize};
use std::process::Command;

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct Server {
    #[serde(skip_deserializing, default)]
    pub id: String,
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

impl ConfigEntity for Server {
    const ENTITY_TYPE: &'static str = "server";
    const DIR_NAME: &'static str = "servers";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> Error {
        Error::server_not_found(id, suggestions)
    }
}

// ============================================================================
// Core CRUD - Thin wrappers around config module
// ============================================================================

pub fn load(id: &str) -> Result<Server> {
    config::load::<Server>(id)
}

pub fn list() -> Result<Vec<Server>> {
    config::list::<Server>()
}

pub fn save(server: &Server) -> Result<()> {
    config::save(server)
}

pub fn delete(id: &str) -> Result<()> {
    config::delete::<Server>(id)
}

pub fn exists(id: &str) -> bool {
    config::exists::<Server>(id)
}

pub fn find_by_host(host: &str) -> Option<Server> {
    list().ok()?.into_iter().find(|s| s.host == host)
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<Server>(id, json_spec, replace_fields)
}

pub fn remove_from_json(id: Option<&str>, json_spec: &str) -> Result<RemoveResult> {
    config::remove_from_json::<Server>(id, json_spec)
}

pub fn create(json_spec: &str, skip_existing: bool) -> Result<CreateOutput<Server>> {
    config::create::<Server>(json_spec, skip_existing)
}

pub fn key_path(id: &str) -> Result<std::path::PathBuf> {
    paths::key(id)
}

// ============================================================================
// Operations
// ============================================================================

#[derive(Debug, Clone)]
pub struct UpdateResult {
    pub id: String,
    pub server: Server,
    pub updated_fields: Vec<String>,
}

pub fn update(
    server_id: &str,
    host: Option<String>,
    user: Option<String>,
    port: Option<u16>,
) -> Result<UpdateResult> {
    let mut server = load(server_id)?;
    let mut updated = Vec::new();

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

pub fn rename(id: &str, new_id: &str) -> Result<Server> {
    let new_id = new_id.to_lowercase();
    config::rename::<Server>(id, &new_id)?;
    load(&new_id)
}

pub fn delete_safe(id: &str) -> Result<()> {
    if !exists(id) {
        let suggestions = config::find_similar_ids::<Server>(id);
        return Err(Error::server_not_found(id.to_string(), suggestions));
    }

    let projects = project::list().unwrap_or_default();
    for proj in projects {
        if proj.server_id.as_deref() == Some(id) {
            return Err(Error::validation_invalid_argument(
                "server",
                format!(
                    "Server is used by project '{}'. Update or delete the project first.",
                    proj.id
                ),
                Some(id.to_string()),
                Some(vec![proj.id.clone()]),
            ));
        }
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
// SSH Key Management
// ============================================================================

#[derive(Debug, Clone, Serialize)]

pub struct KeyGenerateResult {
    pub server: Server,
    pub public_key: String,
    pub identity_file: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct KeyImportResult {
    pub server: Server,
    pub public_key: String,
    pub identity_file: String,
    pub imported_from: String,
}

pub fn generate_key(server_id: &str) -> Result<KeyGenerateResult> {
    load(server_id)?;

    let key_path = key_path(server_id)?;
    let key_path_str = key_path.to_string_lossy().to_string();

    if let Some(parent) = key_path.parent() {
        local_files::local().ensure_dir(parent)?;
    }

    // Best effort cleanup: files may not exist, ignore removal errors
    let _ = std::fs::remove_file(&key_path);
    let _ = std::fs::remove_file(format!("{}.pub", key_path_str));

    let output = Command::new("ssh-keygen")
        .args([
            "-t",
            "rsa",
            "-b",
            "4096",
            "-f",
            &key_path_str,
            "-N",
            "",
            "-C",
            &format!("homeboy-{}", server_id),
        ])
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), Some("run ssh-keygen".to_string())))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::internal_unexpected(format!(
            "ssh-keygen failed: {}",
            stderr
        )));
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

pub fn import_key(server_id: &str, source_path: &str) -> Result<KeyImportResult> {
    load(server_id)?;

    let expanded_path = shellexpand::tilde(source_path).to_string();

    let private_key = std::fs::read_to_string(&expanded_path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read ssh private key".to_string())))?;

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
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |e| Error::internal_io(e.to_string(), Some("set ssh key permissions".to_string())),
        )?;
    }

    std::fs::write(format!("{}.pub", key_path_str), &public_key)
        .map_err(|e| Error::internal_io(e.to_string(), Some("write ssh public key".to_string())))?;

    let server = set_identity_file(server_id, Some(key_path_str.clone()))?;

    Ok(KeyImportResult {
        server,
        public_key,
        identity_file: key_path_str,
        imported_from: expanded_path,
    })
}

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

pub fn unset_key(server_id: &str) -> Result<Server> {
    set_identity_file(server_id, None)
}
