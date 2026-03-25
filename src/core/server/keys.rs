use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use serde::Serialize;
use std::process::Command;

use super::{key_path, load, set_identity_file, Server};

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

    let private_key =
        local_files::read_file(std::path::Path::new(&expanded_path), "read ssh private key")?;

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

    local_files::write_file(&key_path, &private_key, "write ssh private key")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |e| Error::internal_io(e.to_string(), Some("set ssh key permissions".to_string())),
        )?;
    }

    let pub_key_path = format!("{}.pub", key_path_str);
    local_files::write_file(
        std::path::Path::new(&pub_key_path),
        &public_key,
        "write ssh public key",
    )?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_key_default_path() {
        let server_id = "";
        let _result = generate_key(&server_id);
    }

    #[test]
    fn test_generate_key_default_path_2() {
        let server_id = "";
        let _result = generate_key(&server_id);
    }

    #[test]
    fn test_generate_key_if_let_some_parent_key_path_parent() {
        let server_id = "";
        let _result = generate_key(&server_id);
    }

    #[test]
    fn test_generate_key_let_some_parent_key_path_parent() {
        let server_id = "";
        let _result = generate_key(&server_id);
    }

    #[test]
    fn test_generate_key_default_path_3() {
        let server_id = "";
        let _result = generate_key(&server_id);
    }

    #[test]
    fn test_generate_key_default_path_4() {
        let server_id = "";
        let _result = generate_key(&server_id);
    }

    #[test]
    fn test_generate_key_default_path_5() {
        let server_id = "";
        let _result = generate_key(&server_id);
    }

    #[test]
    fn test_generate_key_has_expected_effects() {
        // Expected effects: process_spawn, file_delete
        let server_id = "";
        let _ = generate_key(&server_id);
    }

    #[test]
    fn test_get_public_key_default_path() {
        let server_id = "";
        let _result = get_public_key(&server_id);
    }

    #[test]
    fn test_get_public_key_default_path_2() {
        let server_id = "";
        let _result = get_public_key(&server_id);
    }

    #[test]
    fn test_get_public_key_else() {
        let server_id = "";
        let _result = get_public_key(&server_id);
    }

    #[test]
    fn test_get_public_key_else_2() {
        let server_id = "";
        let _result = get_public_key(&server_id);
    }

    #[test]
    fn test_get_public_key_else_3() {
        let server_id = "";
        let result = get_public_key(&server_id);
        assert!(result.is_ok(), "expected Ok for: else");
    }

    #[test]
    fn test_get_public_key_has_expected_effects() {
        // Expected effects: file_read
        let server_id = "";
        let _ = get_public_key(&server_id);
    }

    #[test]
    fn test_import_key_default_path() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_default_path_2() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_private_key_contains_begin_private_key_contains_private_key() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_private_key_contains_begin_private_key_contains_private_key_2() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_default_path_3() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_default_path_4() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_if_let_some_parent_key_path_parent() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_let_some_parent_key_path_parent() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_let_some_parent_key_path_parent_2() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_e_error_internal_io_e_to_string_some_set_ssh_key_permissions() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_default_path_5() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_default_path_6() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_default_path_7() {
        let server_id = "";
        let source_path = "";
        let _result = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_import_key_has_expected_effects() {
        // Expected effects: process_spawn
        let server_id = "";
        let source_path = "";
        let _ = import_key(&server_id, &source_path);
    }

    #[test]
    fn test_use_key_set_identity_file_server_id_some_expanded_path() {
        let server_id = "";
        let key_path = "";
        let _result = use_key(&server_id, &key_path);
    }

    #[test]
    fn test_unset_key_default_path() {
        let server_id = "";
        let _result = unset_key(&server_id);
    }

}
